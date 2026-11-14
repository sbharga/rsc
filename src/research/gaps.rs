//! Opt-in follow-up round: a reviewer reads the first-round reports and proposes extra researchers.

use super::*;

const MAX_GAP_AGENTS: usize = 4;

fn gap_instructions() -> String {
    format!(
        "You review the first round of a multi-researcher investigation before the final report is written. Read the problem and the researchers' reports. In `notes`, list as a short Markdown list the important sub-questions the reports leave unanswered and the contradictions between them that matter for the user's question. In `gaps`, propose up to {MAX_GAP_AGENTS} follow-up researchers, each with a short title and a 2–3 sentence self-contained brief: what to find out, what the first round already established, and which gap or contradiction it resolves. Return an empty `gaps` list if the reports already answer the question well; never propose work that repeats the first round."
    )
}

/// `(notes, gaps)`; `None` when the text is not the expected JSON.
fn parse_gaps(text: &str) -> Option<(String, Vec<Perspective>)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let notes = v["notes"].as_str().unwrap_or("").trim().to_string();
    let gaps = v["gaps"]
        .as_array()?
        .iter()
        .filter_map(|g| {
            let title = g["title"].as_str()?.trim().to_string();
            (!title.is_empty())
                .then(|| Perspective { title, brief: g["brief"].as_str().unwrap_or("").trim().to_string() })
        })
        .take(MAX_GAP_AGENTS)
        .collect();
    Some((notes, gaps))
}

/// Append `ps` as follow-up researchers; returns their agent indices.
fn add_followups(s: &mut RunState, ps: &[Perspective]) -> Vec<usize> {
    let start = s.record.agents.len();
    for p in ps {
        s.record.agents.push(AgentRec {
            title: p.title.clone(),
            brief: p.brief.clone(),
            status: AgentStatus::Pending,
            error: None,
            usage: Usage::default(),
            cost_usd: None,
            turns: 0,
            followup: true,
        });
        s.activity.push(String::new());
        s.logs.push(vec![]);
        s.reports.push(None);
    }
    s.record.perspectives.extend(ps.iter().cloned());
    (start..s.record.agents.len()).collect()
}

impl Run {
    async fn review_gaps(self: &Arc<Self>) -> Result<(), ApiError> {
        let (brief, sm, agents, reports, budget) = self.with(|s| {
            (
                task_brief(&s.record),
                s.record.config.planner.clone(),
                s.record.agents.clone(),
                s.reports.clone(),
                s.record.config.synth_input_budget_tokens,
            )
        });
        let done = agents.iter().filter(|a| a.status == AgentStatus::Done).count().max(1) as u64;
        let max_chars = (budget / done * 4) as usize;
        let mut input = format!("Problem:\n{brief}\n\n");
        for (i, a) in agents.iter().enumerate() {
            let body = if a.status == AgentStatus::Done {
                let r = reports.get(i).cloned().flatten().unwrap_or_else(|| "(no report)".into());
                tools::truncate_chars(&r, max_chars)
            } else {
                format!("(this researcher failed: {})", a.error.clone().unwrap_or_default())
            };
            input.push_str(&format!("## Report {}: {}\n{}\n\n", i + 1, a.title, body));
        }
        let schema = json!({"type":"json_schema","name":"gaps","strict":true,"schema":{
            "type":"object",
            "properties":{
                "notes":{"type":"string"},
                "gaps":{"type":"array","items":{
                    "type":"object",
                    "properties":{"title":{"type":"string"},"brief":{"type":"string"}},
                    "required":["title","brief"],"additionalProperties":false}}},
            "required":["notes","gaps"],"additionalProperties":false}});
        let req = ResponseRequest {
            model: sm.model,
            instructions: gap_instructions(),
            input: vec![user_msg(&input)],
            tools: vec![],
            effort: Some(sm.effort),
            text: Some(json!({ "format": schema })),
            context_management: None,
            reasoning_summary: false,
        };
        let done = self.call(&req, None).await?;
        self.add_usage(Scope::Planner, done.usage);
        let (notes, gaps) = parse_gaps(&output_text(&done.output))
            .ok_or_else(|| ApiError::new(ErrKind::Fatal, "Gap reviewer returned invalid output"))?;
        self.with(|s| {
            s.record.gaps = gaps;
            s.record.gap_notes = notes;
            s.record.gaps_reviewed = true;
        });
        self.persist();
        Ok(())
    }

    /// After the researchers finish: optionally review for gaps, then synthesize.
    pub(super) async fn after_round(self: Arc<Self>) {
        let review = self.with(|s| s.record.config.gap_round && !s.record.gaps_reviewed && !s.record.budget_hit);
        if review {
            match self.review_gaps().await {
                Ok(()) if self.with(|s| !s.record.gaps.is_empty()) => {
                    self.set_status(RunStatus::GapReview);
                    self.notify(RunEvent::GapsReady);
                    return;
                }
                Ok(()) => {}
                Err(e) if e.kind == ErrKind::Cancelled => return,
                Err(e) if e.kind == ErrKind::UsageLimit => {
                    self.fail(e);
                    return;
                }
                Err(e) => {
                    self.notify(RunEvent::Toast(format!("Gap review failed: {} — writing the report", e.message)));
                    self.with(|s| s.record.gaps_reviewed = true);
                    self.persist();
                }
            }
        }
        self.finish().await;
    }

    /// Chat deep-dive: run `angles` on a finished run and write a new report revision.
    /// The previous `report.md` is kept as `report.v<n>.md`; synthesis then covers every report.
    pub fn deepen(ctx: Ctx, mut record: Record, question: String, angles: Vec<Perspective>) -> Arc<Run> {
        let cur = record.revision.max(1);
        record.revision = cur + 1;
        record.followups.push(question);
        // Straight to synthesis after the new researchers; no second gap review.
        record.gaps_reviewed = true;
        record.error = None;
        let dir = record.dir();
        let old = record.read_final();
        // The proposal in the chat log is consumed.
        let mut log = record.read_chat();
        if log.messages.iter().any(|m| !m.research.is_empty()) {
            log.messages.iter_mut().for_each(|m| m.research.clear());
            let _ = record.write_chat(&log);
        }
        let run = Run::new(ctx, record);
        if let Some(old) = old {
            let p = dir.join(format!("report.v{cur}.md"));
            if !p.exists()
                && let Err(e) = crate::paths::atomic_write(&p, old.as_bytes())
            {
                run.notify(RunEvent::Toast(format!("Couldn't archive the report: {e}")));
            }
        }
        let idxs = run.with(|s| {
            for i in 0..s.record.agents.len() {
                if s.record.agents[i].status == AgentStatus::Done {
                    s.reports[i] = s.record.read_agent_report(i);
                }
            }
            let idxs = add_followups(s, &angles);
            s.record.status = RunStatus::Researching;
            idxs
        });
        run.persist();
        tokio::spawn(run.clone().drive(idxs));
        run
    }

    /// From gap review: add the proposed angles as researchers and run them.
    pub fn approve_gaps(self: &Arc<Self>) {
        let new = self.with(|s| {
            if s.record.status != RunStatus::GapReview {
                return None;
            }
            let gaps = std::mem::take(&mut s.record.gaps);
            let idxs = add_followups(s, &gaps);
            if !gaps.is_empty() {
                s.record.status = RunStatus::Researching;
            }
            Some(idxs)
        });
        match new {
            None => {}
            Some(idxs) if idxs.is_empty() => self.skip_gaps(),
            Some(idxs) => {
                self.persist();
                tokio::spawn(self.clone().drive(idxs));
            }
        }
    }

    /// From gap review: drop the proposal and write the report.
    pub fn skip_gaps(self: &Arc<Self>) {
        let ok = self.with(|s| {
            let ok = s.record.status == RunStatus::GapReview;
            if ok {
                s.record.gaps.clear();
            }
            ok
        });
        if ok {
            self.persist();
            let run = self.clone();
            tokio::spawn(async move { run.finish().await });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gaps_trims_and_caps() {
        let t = r#"{"notes":" n ","gaps":[{"title":" A ","brief":" b "},{"title":" ","brief":"x"},
            {"title":"B","brief":"b"},{"title":"C","brief":"c"},{"title":"D","brief":"d"},{"title":"E","brief":"e"}]}"#;
        let (n, g) = parse_gaps(t).unwrap();
        assert_eq!(n, "n");
        assert_eq!(g.len(), MAX_GAP_AGENTS);
        assert_eq!(g[0], Perspective { title: "A".into(), brief: "b".into() });
        assert_eq!(g[3].title, "D");
    }

    #[test]
    fn parse_gaps_rejects_invalid() {
        assert!(parse_gaps("nope").is_none());
        assert!(parse_gaps(r#"{"notes":"n"}"#).is_none());
        assert_eq!(parse_gaps(r#"{"notes":"n","gaps":[]}"#).unwrap().1, vec![]);
    }
}
