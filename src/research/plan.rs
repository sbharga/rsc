//! Planner and clarification loop.

use super::*;
use crate::config::{AUTO_MAX_AGENTS, AUTO_MIN_AGENTS, MAX_AGENTS};
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
    s.push_str(&context::block(&r.context));
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

fn parse_plan(text: &str, force: bool, max: usize) -> Option<PlanReply> {
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
                .take(max)
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

/// Planner system prompt. `n` researchers exactly, or `0` = the planner chooses within the auto bounds.
fn planner_instructions(template: &str, n: u8, force: bool, mode: RunMode) -> String {
    let (team, step3) = if n > 0 {
        (
            format!("a team of {n} independent researchers"),
            format!(
                "return exactly {n} `perspectives`, each with a short title and a 2–3 sentence brief telling that researcher precisely what to investigate and what to leave to the others."
            ),
        )
    } else {
        (
            "a team of independent researchers".to_string(),
            format!(
                "return between {AUTO_MIN_AGENTS} and {AUTO_MAX_AGENTS} `perspectives`, as many as the request has genuinely distinct parts: 2–3 for a narrow factual lookup, 4–6 for a comparison or multi-part question, up to {AUTO_MAX_AGENTS} for a broad or contested topic. Each has a short title and a 2–3 sentence brief telling that researcher precisely what to investigate and what to leave to the others; angles must not overlap."
            ),
        )
    };
    let mut s = prompts::fill(template, &[("team", &team), ("step3", &step3)]);
    s.push_str(&prompts::discover(mode, prompts::DISCOVER_PLANNER));
    if force {
        s.push_str("\n\nDo not ask any more questions: `questions` must be empty and you must return the perspectives now, making reasonable assumptions where needed.");
    }
    s
}

const STOPWORDS: [&str; 33] = [
    "this",
    "that",
    "with",
    "from",
    "what",
    "which",
    "their",
    "about",
    "into",
    "than",
    "them",
    "they",
    "have",
    "will",
    "each",
    "other",
    "also",
    "such",
    "more",
    "most",
    "should",
    "these",
    "those",
    "researcher",
    "researchers",
    "investigate",
    "focus",
    "including",
    "does",
    "when",
    "where",
    "while",
    "between",
];

fn angle_words(p: &Perspective) -> std::collections::HashSet<String> {
    format!("{} {}", p.title, p.brief)
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4 && !STOPWORDS.contains(w))
        .map(String::from)
        .collect()
}

/// Pairs (i < j) of angles whose wording overlaps enough to suggest duplicate work.
pub fn overlaps(ps: &[Perspective]) -> Vec<(usize, usize)> {
    let words: Vec<_> = ps.iter().map(angle_words).collect();
    let mut out = vec![];
    for i in 0..ps.len() {
        for j in i + 1..ps.len() {
            if words[i].is_empty() || words[j].is_empty() {
                continue;
            }
            let both = words[i].intersection(&words[j]).count() as f64;
            let either = words[i].union(&words[j]).count() as f64;
            if both / either >= 0.4 {
                out.push((i, j));
            }
        }
    }
    out
}

impl Run {
    /// Run the planner again (also used for the first plan).
    pub fn replan(self: &Arc<Self>) {
        tokio::spawn(self.clone().plan_task(true));
    }

    // ---- planner ----

    pub(super) async fn plan_task(self: Arc<Self>, force: bool) {
        self.set_step("");
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
        let (input, n, sm, rounds, template, mode) = self.with(|s| {
            (
                planner_input(&s.record),
                s.record.config.agents,
                s.record.config.planner.clone(),
                s.record.clarify_rounds,
                s.record.prompts.planner().to_string(),
                s.record.config.mode,
            )
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
        let instructions = planner_instructions(&template, n, force, mode);
        let max = if n == 0 { AUTO_MAX_AGENTS } else { MAX_AGENTS } as usize;
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
        let r = parse_plan(&text, force, max)
            .ok_or_else(|| ApiError::new(ErrKind::Fatal, "Planner returned invalid plan"))?;
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
        let q = parse_plan(t, false, MAX_AGENTS as usize).unwrap();
        assert_eq!(q.understanding, "u");
        assert_eq!(q.outcome, PlanOutcome::Questions(vec!["a".into(), "b".into(), "c".into()]));
        let p = parse_plan(t, true, MAX_AGENTS as usize).unwrap();
        assert_eq!(p.outcome, PlanOutcome::Plan(vec![Perspective { title: "T".into(), brief: "B".into() }]));
    }

    #[test]
    fn parse_plan_rejects_empty() {
        let t = r#"{"understanding":"u","notes":"","questions":[],"perspectives":[]}"#;
        assert!(parse_plan(t, false, MAX_AGENTS as usize).is_none());
        assert!(parse_plan(t, true, MAX_AGENTS as usize).is_none());
    }

    #[test]
    fn planner_instructions_auto_vs_pinned() {
        let pinned = planner_instructions(prompts::PLANNER, 3, false, RunMode::Research);
        assert!(pinned.contains("exactly 3 `perspectives`"));
        let auto = planner_instructions(prompts::PLANNER, 0, true, RunMode::Research);
        assert!(auto.contains("between 2 and 8") && !auto.contains("return exactly"));
        assert!(auto.contains("Do not ask any more questions"));
    }

    fn angle(t: &str, b: &str) -> Perspective {
        Perspective { title: t.into(), brief: b.into() }
    }

    #[test]
    fn overlaps_flags_similar_angles_only() {
        let a = angle(
            "Solid-state battery costs",
            "Estimate manufacturing costs of solid-state battery cells and how they compare with lithium-ion cells.",
        );
        let b = angle(
            "Solid-state battery cost outlook",
            "Estimate manufacturing costs for solid-state battery cells and compare them with lithium-ion cells over time.",
        );
        let c = angle("Regulation in the EU", "Summarize EU rules on battery recycling and import tariffs.");
        assert_eq!(overlaps(&[a, b, c]), vec![(0, 1)]);
    }
}
