//! Synthesis of the final report.

use super::*;

const BUDGET_FOOTER: &str =
    "\n\n---\n\n*Research stopped early because the run budget was reached; some angles may be incomplete.*";
const SYNTH_INSTRUCTIONS: &str = "Several researchers investigated the problem below from different angles. Combine their reports into one final answer in Markdown: integrate findings and resolve or flag conflicts. Cite sources by their number from the Sources list in square brackets, e.g. [3] or [2, 5]. If you use a source that is not on the list, cite it as a Markdown link [title](url). Do not write your own sources or references section; one is added automatically. You may search the web to verify key claims.";

fn synth_instructions(style: &str) -> String {
    format!("{SYNTH_INSTRUCTIONS}{}", style_suffix(style))
}

impl Run {
    async fn condense(&self, report: &str, words: u64) -> Result<String, ApiError> {
        let sm = self.with(|s| s.record.config.compactor.clone());
        let done = self
            .call(
                &ResponseRequest {
                    model: sm.model,
                    instructions: format!(
                        "Condense this research report to at most {words} words. Keep findings, figures, disagreements and source URLs; drop repetition."
                    ),
                    input: vec![user_msg(report)],
                    tools: vec![],
                    effort: Some(sm.effort),
                    text: None,
                    context_management: None,
                    reasoning_summary: false,
                },
                None,
            )
            .await?;
        self.add_usage(Scope::Compaction, done.usage);
        Ok(output_text(&done.output))
    }

    // ---- synthesis ----

    pub(super) async fn synthesize(self: &Arc<Self>) -> Result<String, ApiError> {
        let (brief, cfg, agents, mem_reports, list) = self.with(|s| {
            // Drop synthesizer-only sources and citations from any previous attempt.
            s.record.sources.retain(|x| !x.agents.is_empty());
            for x in &mut s.record.sources {
                x.in_final = false;
            }
            let list = sources::render_list(&s.record.sources, |x| {
                x.agents.iter().any(|&i| s.record.agents.get(i).is_some_and(|a| a.status == AgentStatus::Done))
            });
            (task_brief(&s.record), s.record.config.clone(), s.record.agents.clone(), s.reports.clone(), list)
        });
        let ok = agents.iter().filter(|a| a.status == AgentStatus::Done).count().max(1) as u64;
        let per_report = cfg.synth_input_budget_tokens / ok;
        let mut jobs = vec![];
        for (i, a) in agents.iter().enumerate() {
            let run = self.clone();
            let report = mem_reports.get(i).cloned().flatten();
            let a = a.clone();
            jobs.push(async move {
                let body = if a.status == AgentStatus::Done {
                    let text = report.unwrap_or_else(|| "(no report)".into());
                    if (text.chars().count() as u64) / 4 > per_report {
                        run.set_activity(i, "condensing");
                        run.condense(&text, per_report * 3 / 4).await?
                    } else {
                        text
                    }
                } else {
                    format!("(this researcher failed: {})", a.error.clone().unwrap_or_default())
                };
                Ok::<_, ApiError>(format!("## Report {}: {}\n{}\n\n", i + 1, a.title, body))
            });
        }
        let parts = futures_util::future::join_all(jobs).await.into_iter().collect::<Result<Vec<_>, _>>()?;
        for i in 0..agents.len() {
            self.set_activity(i, "");
        }
        let block = if list.is_empty() { String::new() } else { format!("## Sources\n{list}") };
        let input = format!("Problem:\n{brief}\n\n{}{block}", parts.concat());
        let sm = &cfg.synthesizer;
        let req = ResponseRequest {
            model: sm.model.clone(),
            instructions: synth_instructions(&cfg.report_style),
            input: vec![user_msg(&input)],
            tools: vec![tools::web_search_tool()],
            effort: Some(sm.effort.clone()),
            text: None,
            context_management: None,
            reasoning_summary: false,
        };
        let tx = self.forwarder(Target::Synth);
        self.with(|s| s.synth_text.clear());
        let done = self.call(&req, Some(&tx)).await?;
        self.add_usage(Scope::Synth, done.usage);
        let text = output_text(&done.output);
        let text = if text.trim().is_empty() { self.with(|s| s.synth_text.clone()) } else { text };
        let section = self.with(|s| {
            sources::mark_cited(&mut s.record.sources, &text);
            sources::merge(
                &mut s.record.sources,
                [sources::annotations(&done.output), sources::links(&text)].concat(),
                None,
            );
            sources::render_list(&s.record.sources, |x| x.in_final)
        });
        let mut out = if section.is_empty() { text } else { format!("{}\n\n## Sources\n\n{section}", text.trim_end()) };
        if self.with(|s| s.record.budget_hit) {
            out.push_str(BUDGET_FOOTER);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_instructions_append_style() {
        assert_eq!(synth_instructions("  "), SYNTH_INSTRUCTIONS);
        let s = synth_instructions("Explain like I'm 5");
        assert!(s.starts_with(SYNTH_INSTRUCTIONS) && s.ends_with("Explain like I'm 5"));
    }
}
