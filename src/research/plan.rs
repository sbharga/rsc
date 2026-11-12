//! Planner and clarification loop.

use super::*;
use crate::config::MAX_AGENTS;
use store::Clarification;

const MAX_CLARIFY_ROUNDS: u8 = 2;

fn planner_input(r: &Record) -> String {
    let mut s = format!("Request:\n{}", r.question);
    if !r.planner_notes.trim().is_empty() {
        s.push_str(&format!("\n\nYour notes from earlier research:\n{}", r.planner_notes));
    }
    if !r.clarifications.is_empty() {
        s.push_str("\n\nThe user answered your questions:\n");
        for c in &r.clarifications {
            s.push_str(&format!("Q: {}\nA: {}\n", c.question, c.answer));
        }
    }
    s
}

#[derive(Debug, PartialEq)]
enum PlanOutcome {
    Questions(Vec<String>),
    Plan(Vec<Perspective>),
}

struct PlanReply {
    understanding: String,
    notes: String,
    outcome: PlanOutcome,
}

fn parse_plan(text: &str, force: bool) -> Option<PlanReply> {
    let v: Value = serde_json::from_str(text).ok()?;
    let field = |k: &str| v[k].as_str().unwrap_or("").trim().to_string();
    let questions: Vec<String> = v["questions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|q| q.as_str())
                .map(|q| q.trim().to_string())
                .filter(|q| !q.is_empty())
                .take(3)
                .collect()
        })
        .unwrap_or_default();
    let ps: Vec<Perspective> = v["perspectives"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some(Perspective {
                        title: p["title"].as_str()?.trim().to_string(),
                        brief: p["brief"].as_str()?.trim().to_string(),
                    })
                })
                .take(MAX_AGENTS as usize)
                .collect()
        })
        .unwrap_or_default();
    let outcome = if !force && !questions.is_empty() {
        PlanOutcome::Questions(questions)
    } else if !ps.is_empty() {
        PlanOutcome::Plan(ps)
    } else {
        return None;
    };
    Some(PlanReply { understanding: field("understanding"), notes: field("notes"), outcome })
}

impl Run {
    /// Run the planner again (also used for the first plan).
    pub fn replan(self: &Arc<Self>) {
        tokio::spawn(self.clone().plan_task(true));
    }

    // ---- planner ----

    pub(super) async fn plan_task(self: Arc<Self>, force: bool) {
        self.with(|s| s.record.error = None);
        self.note(Target::Planner, "planning");
        self.set_status(RunStatus::Planning);
        match self.plan(force).await {
            Ok(PlanOutcome::Questions(qs)) => {
                self.with(|s| s.record.questions = qs);
                self.set_status(RunStatus::Clarifying);
                self.notify(RunEvent::Questions);
            }
            Ok(PlanOutcome::Plan(ps)) => {
                self.with(|s| {
                    s.record.perspectives = ps;
                    s.record.questions.clear();
                });
                self.set_status(RunStatus::Review);
                self.notify(RunEvent::PlanReady);
            }
            Err(e) => self.fail(e),
        }
    }

    /// From clarifying: record the user's answers (all blank = skip) and plan again.
    pub fn answer(self: &Arc<Self>, answers: Vec<String>) {
        if self.status() != RunStatus::Clarifying {
            return;
        }
        let skipped = answers.iter().all(|a| a.trim().is_empty());
        self.with(|s| {
            let qs = std::mem::take(&mut s.record.questions);
            if !skipped {
                for (question, a) in qs.into_iter().zip(answers) {
                    let a = a.trim();
                    let answer =
                        if a.is_empty() { "(no answer — use your judgment)".to_string() } else { a.to_string() };
                    s.record.clarifications.push(Clarification { question, answer });
                }
            }
            s.record.clarify_rounds += 1;
            s.record.status = RunStatus::Planning;
        });
        self.persist();
        tokio::spawn(self.clone().plan_task(skipped));
    }

    async fn plan(self: &Arc<Self>, force: bool) -> Result<PlanOutcome, ApiError> {
        let (input, n, sm, rounds) = self.with(|s| {
            (planner_input(&s.record), s.record.config.agents, s.record.config.planner.clone(), s.record.clarify_rounds)
        });
        let force = force || rounds >= MAX_CLARIFY_ROUNDS;
        let schema = json!({"type":"json_schema","name":"plan","strict":true,"schema":{
            "type":"object",
            "properties":{
                "understanding":{"type":"string"},
                "notes":{"type":"string"},
                "questions":{"type":"array","items":{"type":"string"}},
                "perspectives":{"type":"array","items":{
                    "type":"object",
                    "properties":{"title":{"type":"string"},"brief":{"type":"string"}},
                    "required":["title","brief"],"additionalProperties":false}}},
            "required":["understanding","notes","questions","perspectives"],"additionalProperties":false}});
        let mut instructions = format!(
            "You plan research for a team of {n} independent researchers. First make sure you understand exactly what the user wants, then give each researcher a distinct assignment.\n\n\
1. Do light research: at most 3 quick web searches to learn key terms, the current state of the topic and what matters for scoping. Do not research in depth — the researchers will.\n\
2. If the request is ambiguous in a way that would change what the researchers should do (scope, audience, timeframe, region, depth, constraints, or the kind of answer wanted), put up to 3 short, specific questions in `questions` and leave `perspectives` empty. Do not ask about anything you can reasonably assume.\n\
3. Otherwise leave `questions` empty and return exactly {n} `perspectives`, each with a short title and a 2–3 sentence brief telling that researcher precisely what to investigate and what to leave to the others.\n\n\
Always fill `understanding` with 1–3 sentences stating what the user is asking for, including any constraints from their answers, and `notes` with the key facts you found (with source URLs) that shaped the plan."
        );
        if force {
            instructions.push_str("\n\nDo not ask any more questions: `questions` must be empty and you must return the perspectives now, making reasonable assumptions where needed.");
        }
        let req = ResponseRequest {
            model: sm.model.clone(),
            instructions,
            input: vec![user_msg(&input)],
            tools: vec![tools::web_search_tool()],
            effort: Some(sm.effort),
            text: Some(json!({ "format": schema })),
            context_management: None,
            reasoning_summary: self.reasoning_summary.load(Ordering::Relaxed),
        };
        let done = self.call(&req, Some(&self.forwarder(Target::Planner))).await?;
        self.add_usage(Scope::Planner, done.usage);
        let text = output_text(&done.output);
        let r =
            parse_plan(&text, force).ok_or_else(|| ApiError::new(ErrKind::Fatal, "Planner returned invalid plan"))?;
        self.with(|s| {
            if !r.understanding.is_empty() {
                s.record.understanding = r.understanding;
            }
            if !r.notes.is_empty() {
                s.record.planner_notes = r.notes;
            }
        });
        Ok(r.outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plan_prefers_questions_unless_forced() {
        let t = r#"{"understanding":" u ","notes":"n","questions":["a","b","c","d"," "],"perspectives":[{"title":"T","brief":"B"}]}"#;
        let q = parse_plan(t, false).unwrap();
        assert_eq!(q.understanding, "u");
        assert_eq!(q.outcome, PlanOutcome::Questions(vec!["a".into(), "b".into(), "c".into()]));
        let p = parse_plan(t, true).unwrap();
        assert_eq!(p.outcome, PlanOutcome::Plan(vec![Perspective { title: "T".into(), brief: "B".into() }]));
    }

    #[test]
    fn parse_plan_rejects_empty() {
        let t = r#"{"understanding":"u","notes":"","questions":[],"perspectives":[]}"#;
        assert!(parse_plan(t, false).is_none());
        assert!(parse_plan(t, true).is_none());
    }
}
