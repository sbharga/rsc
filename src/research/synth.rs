//! Synthesis of the final report.

use super::store::{LinkStatus, Source};
use super::*;

const BUDGET_FOOTER: &str =
    "\n\n---\n\n*Research stopped early because the run budget was reached; some angles may be incomplete.*";

/// More finished reports than this are synthesized in groups first.
const HIER_THRESHOLD: usize = 8;
const GROUP_SIZE: usize = 6;
const GROUP_INSTRUCTIONS: &str = "Merge these researcher reports into one detailed intermediate summary for a writer who will combine it with summaries of other report groups. Keep every finding, figure, date, disagreement and open question; drop repetition. Cite sources by their number from the Sources list in square brackets, e.g. [3], keeping the numbers exactly; cite a source that is not on the list as a Markdown link [title](url). Do not add a sources section.";

fn synth_instructions(template: &str, style: &str) -> String {
    format!("{template}{}", style_suffix(style))
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

    /// Synthesize, store and announce the final report.
    pub(super) async fn finish(self: &Arc<Self>) {
        self.set_status(RunStatus::Synthesizing);
        match self.synthesize().await {
            Ok(text) => {
                let rec = self.with(|s| {
                    s.final_report = Some(text.clone());
                    s.record.status = RunStatus::Done;
                    s.record.clone()
                });
                if let Err(e) = rec.write_final(&text) {
                    self.notify(RunEvent::Toast(format!("Couldn't save report: {e}")));
                }
                self.persist();
                self.notify(RunEvent::Done);
            }
            Err(e) => self.fail(e),
        }
    }

    /// `## Report n: title` blocks for the given agents; long reports are condensed to `per_report` tokens.
    async fn report_parts(
        self: &Arc<Self>,
        idxs: &[usize],
        agents: &[AgentRec],
        mem: &[Option<String>],
        per_report: u64,
    ) -> Result<String, ApiError> {
        let mut jobs = vec![];
        for &i in idxs {
            let run = self.clone();
            let report = mem.get(i).cloned().flatten();
            let a = agents[i].clone();
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
        for &i in idxs {
            self.set_activity(i, "");
        }
        Ok(parts.concat())
    }

    pub(super) async fn synthesize(self: &Arc<Self>) -> Result<String, ApiError> {
        let (brief, cfg, agents, mem_reports, srcs, list, listed, done) = self.with(|s| {
            // Drop synthesizer-only sources and citations from any previous attempt.
            s.record.sources.retain(|x| !x.agents.is_empty());
            for x in &mut s.record.sources {
                x.in_final = false;
                x.status = LinkStatus::Unchecked;
                x.verdicts.clear();
            }
            let keep = |x: &Source| {
                x.agents.iter().any(|&i| s.record.agents.get(i).is_some_and(|a| a.status == AgentStatus::Done))
            };
            let list = sources::render_list(&s.record.sources, keep);
            let listed: Vec<usize> =
                s.record.sources.iter().enumerate().filter(|(_, x)| keep(x)).map(|(i, _)| i + 1).collect();
            let done: Vec<usize> = s
                .record
                .agents
                .iter()
                .enumerate()
                .filter(|(_, a)| a.status == AgentStatus::Done)
                .map(|(i, _)| i)
                .collect();
            (
                task_brief(&s.record),
                s.record.config.clone(),
                s.record.agents.clone(),
                s.reports.clone(),
                s.record.sources.clone(),
                list,
                listed,
                done,
            )
        });
        let budget = cfg.synth_input_budget_tokens;
        let groups = synth_groups(&done);
        let body = if groups.len() == 1 {
            let all: Vec<usize> = (0..agents.len()).collect();
            self.report_parts(&all, &agents, &mem_reports, budget / done.len().max(1) as u64).await?
        } else {
            let group_budget = budget / groups.len() as u64;
            let sm = cfg.synthesizer.clone();
            let mut jobs = vec![];
            for (g, group) in groups.iter().enumerate() {
                let run = self.clone();
                let (agents, mem, brief, sm) = (agents.clone(), mem_reports.clone(), brief.clone(), sm.clone());
                let glist = sources::render_list(&srcs, |x| x.agents.iter().any(|a| group.contains(a)));
                jobs.push(async move {
                    let parts = run.report_parts(group, &agents, &mem, budget / group.len() as u64).await?;
                    for &i in group {
                        run.set_activity(i, "merging");
                    }
                    let req = ResponseRequest {
                        model: sm.model,
                        instructions: format!(
                            "{GROUP_INSTRUCTIONS} Write at most about {} words.",
                            group_budget * 3 / 4
                        ),
                        input: vec![user_msg(&format!("Problem:\n{brief}\n\n{parts}## Sources\n{glist}"))],
                        tools: vec![],
                        effort: Some(sm.effort),
                        text: None,
                        context_management: None,
                        reasoning_summary: false,
                    };
                    let done = run.call(&req, None).await?;
                    run.add_usage(Scope::Synth, done.usage);
                    let mut out = output_text(&done.output);
                    if (out.chars().count() as u64) / 4 > group_budget {
                        out = run.condense(&out, group_budget * 3 / 4).await?;
                    }
                    for &i in group {
                        run.set_activity(i, "");
                    }
                    let nums = group.iter().map(|i| (i + 1).to_string()).collect::<Vec<_>>().join(", ");
                    Ok::<_, ApiError>(format!("## Group {} summary (reports {nums})\n{out}\n\n", g + 1))
                });
            }
            let summaries = futures_util::future::join_all(jobs).await.into_iter().collect::<Result<Vec<_>, _>>()?;
            let failed: Vec<usize> = (0..agents.len()).filter(|i| !done.contains(i)).collect();
            let failed_parts = self.report_parts(&failed, &agents, &mem_reports, 0).await?;
            format!("{}{failed_parts}", summaries.concat())
        };
        let block = if list.is_empty() { String::new() } else { format!("## Sources\n{list}") };
        let input = format!("Problem:\n{brief}\n\n{body}{block}");
        let template = self.with(|s| s.record.prompts.synthesizer().to_string());
        let sm = &cfg.synthesizer;
        let req = ResponseRequest {
            model: sm.model.clone(),
            instructions: synth_instructions(&template, &cfg.report_style),
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
        self.with(|s| {
            sources::mark_cited(&mut s.record.sources, &text);
            s.record.synthesis.invalid_citations = sources::unlisted(&text, &listed);
            sources::merge(&mut s.record.sources, sources::annotations(&done.output), None, true);
            sources::merge(&mut s.record.sources, sources::links(&text), None, false);
        });
        self.check_sources(&text).await?;
        let section = self.with(|s| sources::render_list(&s.record.sources, |x| x.in_final));
        let mut out = if section.is_empty() { text } else { format!("{}\n\n## Sources\n\n{section}", text.trim_end()) };
        if self.with(|s| s.record.budget_hit) {
            out.push_str(BUDGET_FOOTER);
        }
        Ok(out)
    }
}

/// Split the finished reports into ordered groups of near-equal size when there are too many for one pass.
fn synth_groups(done: &[usize]) -> Vec<Vec<usize>> {
    if done.len() <= HIER_THRESHOLD {
        return vec![done.to_vec()];
    }
    let k = done.len().div_ceil(GROUP_SIZE);
    let (base, extra) = (done.len() / k, done.len() % k);
    let mut out = Vec::with_capacity(k);
    let mut at = 0;
    for g in 0..k {
        let n = base + usize::from(g < extra);
        out.push(done[at..at + n].to_vec());
        at += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_instructions_append_style() {
        assert_eq!(synth_instructions(prompts::SYNTHESIZER, "  "), prompts::SYNTHESIZER);
        let s = synth_instructions(prompts::SYNTHESIZER, "Explain like I'm 5");
        assert!(s.starts_with(prompts::SYNTHESIZER) && s.ends_with("Explain like I'm 5"));
    }

    #[test]
    fn synth_groups_balance_and_keep_order() {
        let sizes = |n: usize| {
            let v: Vec<usize> = (0..n).collect();
            let g = synth_groups(&v);
            assert_eq!(g.concat(), v);
            g.iter().map(Vec::len).collect::<Vec<_>>()
        };
        assert_eq!(sizes(0), vec![0]);
        assert_eq!(sizes(8), vec![8]);
        assert_eq!(sizes(9), vec![5, 4]);
        assert_eq!(sizes(13), vec![5, 4, 4]);
        assert_eq!(sizes(30), vec![6, 6, 6, 6, 6]);
    }
}
