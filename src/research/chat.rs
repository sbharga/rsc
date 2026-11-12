//! Follow-up chat about a finished report.

use super::store::{ChatLog, ChatMsg, ChatRole, Record};
use super::{Ctx, RunEvent, output_text, style_suffix, task_brief, tools, user_msg};
use crate::api::{ErrKind, ResponseRequest, StreamDelta};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const CHAT_INSTRUCTIONS: &str = "You wrote the research report below for the user. Answer their follow-up questions about it in Markdown. Base answers on the report and its sources; search the web when a question needs information the report does not cover or to check a claim. Cite sources from the report's Sources list by number in square brackets, e.g. [3]; cite any new source as a Markdown link [title](url). Say so when the report and sources do not support an answer.";

pub struct ChatState {
    pub log: ChatLog,
    /// Question being answered; Some = busy.
    pub pending: Option<String>,
    /// Streamed text of the answer in progress.
    pub stream: String,
}

pub struct Chat {
    ctx: Ctx,
    record: Record,
    report: String,
    state: Mutex<ChatState>,
    pub cancel: CancellationToken,
}

fn assistant_msg(t: &str) -> Value {
    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":t}]})
}

fn chat_request(rec: &Record, report: &str, history: &[ChatMsg], question: &str) -> ResponseRequest {
    let mut input = vec![user_msg(&format!("Problem:\n{}\n\nReport:\n{report}", task_brief(rec)))];
    for m in history {
        input.push(match m.role {
            ChatRole::User => user_msg(&m.text),
            ChatRole::Assistant => assistant_msg(&m.text),
        });
    }
    input.push(user_msg(question));
    ResponseRequest {
        model: rec.config.synthesizer.model.clone(),
        instructions: format!("{CHAT_INSTRUCTIONS}{}", style_suffix(&rec.config.report_style)),
        input,
        tools: vec![tools::web_search_tool()],
        effort: Some(rec.config.synthesizer.effort.clone()),
        text: None,
        context_management: None,
        reasoning_summary: false,
    }
}

impl Chat {
    pub fn open(ctx: Ctx, record: Record, report: String) -> Arc<Chat> {
        let log = record.read_chat();
        Arc::new(Chat {
            ctx,
            record,
            report,
            state: Mutex::new(ChatState { log, pending: None, stream: String::new() }),
            cancel: CancellationToken::new(),
        })
    }

    pub fn id(&self) -> &str {
        &self.record.id
    }

    pub fn question(&self) -> &str {
        &self.record.question
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut ChatState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    pub fn busy(&self) -> bool {
        self.with(|s| s.pending.is_some())
    }

    /// Chat cost in USD; the flag is true when the model has no known rates (only search is priced).
    pub fn cost(&self) -> (f64, bool) {
        let u = self.with(|s| s.log.usage);
        match self.ctx.pricing.cost(&self.record.config.synthesizer.model, &u) {
            Some(c) => (c, false),
            None => (self.ctx.pricing.search_cost(&u), true),
        }
    }

    /// Ask a question; ignored while an answer is in progress.
    pub fn send(self: &Arc<Self>, question: String) {
        let started = self.with(|s| {
            if s.pending.is_some() {
                return false;
            }
            s.pending = Some(question.clone());
            s.stream.clear();
            true
        });
        if started {
            tokio::spawn(self.clone().answer(question));
        }
    }

    async fn answer(self: Arc<Self>, q: String) {
        let req = self.with(|s| chat_request(&self.record, &self.report, &s.log.messages, &q));
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamDelta>();
        let me = self.clone();
        let fwd = tokio::spawn(async move {
            while let Some(d) = rx.recv().await {
                match d {
                    StreamDelta::Text(t) => me.with(|s| s.stream.push_str(&t)),
                    StreamDelta::Restart => me.with(|s| s.stream.clear()),
                    _ => {}
                }
            }
        });
        let r = self.ctx.client.call_with_retry(&req, Some(&tx), &self.cancel).await;
        drop(tx);
        let _ = fwd.await;
        match r {
            Ok(done) => {
                let streamed = self.with(|s| s.stream.clone());
                let mut text = output_text(&done.output);
                if text.trim().is_empty() {
                    text = if streamed.trim().is_empty() { "(no answer)".into() } else { streamed };
                }
                let log = self.with(|s| {
                    s.log.usage += done.usage;
                    s.log.messages.push(ChatMsg { role: ChatRole::User, text: q });
                    s.log.messages.push(ChatMsg { role: ChatRole::Assistant, text });
                    s.pending = None;
                    s.stream.clear();
                    s.log.clone()
                });
                if let Err(e) = self.record.write_chat(&log) {
                    let _ = self.ctx.tx.send(RunEvent::Toast(format!("Couldn't save chat: {e}")));
                }
            }
            Err(e) => {
                self.with(|s| {
                    s.pending = None;
                    s.stream.clear();
                });
                if e.kind != ErrKind::Cancelled {
                    let _ = self.ctx.tx.send(RunEvent::ChatFailed { question: q, error: e });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StageModel;
    use crate::research::store::RunConfig;

    #[test]
    fn request_carries_report_history_and_style() {
        let sm = StageModel { model: "syn".into(), effort: "high".into() };
        let cfg = RunConfig {
            planner: sm.clone(),
            researcher: sm.clone(),
            compactor: sm.clone(),
            synthesizer: sm,
            agents: 1,
            max_parallel: 1,
            fetch_url: false,
            report_words: 0,
            max_turns: 1,
            compact_threshold_tokens: 1,
            synth_input_budget_tokens: 1,
            report_style: "No jargon".into(),
            max_run_tokens: 0,
            max_run_cost_usd: 0.0,
        };
        let rec = Record::new("Q".into(), cfg);
        let hist = [
            ChatMsg { role: ChatRole::User, text: "a".into() },
            ChatMsg { role: ChatRole::Assistant, text: "b".into() },
        ];
        let req = chat_request(&rec, "REPORT", &hist, "c");
        assert_eq!(req.input.len(), 4);
        let first = req.input[0]["content"][0]["text"].as_str().unwrap();
        assert!(first.contains("REPORT") && first.contains('Q'));
        assert_eq!(req.input[1]["role"], "user");
        assert_eq!(req.input[2]["role"], "assistant");
        assert_eq!(req.input[2]["content"][0]["type"], "output_text");
        assert_eq!(req.input[3]["content"][0]["text"], "c");
        assert_eq!(req.model, "syn");
        assert!(req.instructions.ends_with("No jargon"));
        assert_eq!(req.tools[0]["type"], "web_search");
        assert_eq!(req.body()["store"], false);
    }
}
