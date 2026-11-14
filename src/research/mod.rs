//! Research orchestration: planner → (review) → parallel researchers → synthesizer.

mod agent;
pub mod chat;
pub mod context;
mod gaps;
pub mod plan;
pub mod prompts;
pub mod sources;
pub mod store;
mod synth;
pub mod tools;
mod verify;

use crate::api::{ApiError, Client, Completed, ErrKind, ResponseRequest, StreamDelta};
use crate::pricing::{Pricing, Usage};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use store::{AgentRec, AgentStatus, ErrRec, LocalDoc, Perspective, Record, RunConfig, RunMode, RunStatus};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Notifications that need the UI to act (everything else is read from the shared state each frame).
#[derive(Debug)]
pub enum RunEvent {
    PlanReady,
    GapsReady,
    Done,
    Failed(ApiError),
    Toast(String),
    Questions,
    ChatFailed { question: String, error: ApiError },
}

#[derive(Clone, Debug, PartialEq)]
pub enum LogEntry {
    Thinking(String),
    Search(String),
    Open(String),
    Text(String),
    Note(String),
}

const LOG_CAP: usize = 500;

/// New entry; drops oldest beyond `LOG_CAP`.
pub fn push_log(log: &mut Vec<LogEntry>, e: LogEntry) {
    log.push(e);
    if log.len() > LOG_CAP {
        log.drain(..log.len() - LOG_CAP);
    }
}

/// Streaming append: merges into the last entry when both are Thinking or both are Text.
pub fn append_log(log: &mut Vec<LogEntry>, e: LogEntry) {
    match (log.last_mut(), &e) {
        (Some(LogEntry::Thinking(a)), LogEntry::Thinking(b)) | (Some(LogEntry::Text(a)), LogEntry::Text(b)) => {
            a.push_str(b)
        }
        _ => push_log(log, e),
    }
}

#[derive(Clone)]
pub struct Ctx {
    pub client: Client,
    pub pricing: Pricing,
    pub tx: mpsc::UnboundedSender<RunEvent>,
}

pub struct RunState {
    pub record: Record,
    pub activity: Vec<String>,
    pub synth_text: String,
    pub final_report: Option<String>,
    /// In-memory agent reports.
    pub reports: Vec<Option<String>>,
    pub error: Option<ApiError>,
    /// Live timelines (memory only; a resumed run starts empty).
    pub planner_log: Vec<LogEntry>,
    pub logs: Vec<Vec<LogEntry>>,
    /// What the run is doing outside per-agent activity (gap review, synthesis phases, citation checks); empty = the status says enough.
    pub step: String,
    /// Status the clock below belongs to, and when it was first seen.
    stage: (RunStatus, Instant),
}

impl RunState {
    /// Time in the current status; the clock restarts the first time a new status is observed.
    pub fn stage_elapsed(&mut self) -> Duration {
        if self.stage.0 != self.record.status {
            self.stage = (self.record.status, Instant::now());
        }
        self.stage.1.elapsed()
    }

    fn log_for(&mut self, t: &Target) -> Option<&mut Vec<LogEntry>> {
        match t {
            Target::Planner => Some(&mut self.planner_log),
            Target::Agent(i) => self.logs.get_mut(*i),
            Target::Synth => None,
        }
    }
}

pub struct Run {
    ctx: Ctx,
    http: reqwest::Client,
    state: Mutex<RunState>,
    pub cancel: CancellationToken,
    server_compaction: AtomicBool,
    fetch_enabled: AtomicBool,
    reasoning_summary: AtomicBool,
    restarts: AtomicU32,
}

enum Scope {
    Planner,
    Compaction,
    Agent(usize),
    Synth,
    Verify,
}

#[derive(Clone, Copy)]
enum Target {
    Planner,
    Agent(usize),
    Synth,
}

fn user_msg(t: &str) -> Value {
    json!({"type":"message","role":"user","content":[{"type":"input_text","text":t}]})
}

pub fn output_text(items: &[Value]) -> String {
    let mut s = String::new();
    for it in items.iter().filter(|i| i["type"] == "message") {
        if let Some(parts) = it["content"].as_array() {
            for p in parts.iter().filter(|p| p["type"] == "output_text") {
                if let Some(t) = p["text"].as_str() {
                    s.push_str(t);
                }
            }
        }
    }
    s
}

fn first_query(item: &Value) -> Option<String> {
    let a = &item["action"];
    a["query"].as_str().or_else(|| a["queries"][0].as_str()).map(String::from)
}

fn err_param_has(e: &ApiError, needle: &str) -> bool {
    e.param.as_deref().is_some_and(|p| p.contains(needle))
}

/// The problem as researchers and the synthesizer see it: question, planner's understanding, user's answers.
pub fn task_brief(r: &Record) -> String {
    let mut s = r.question.clone();
    if !r.understanding.trim().is_empty() {
        s.push_str(&format!("\n\nWhat the user wants (planner's summary): {}", r.understanding));
    }
    if !r.clarifications.is_empty() {
        s.push_str("\n\nClarifications from the user:\n");
        for c in &r.clarifications {
            s.push_str(&format!("- Q: {} A: {}\n", c.question, c.answer));
        }
    }
    if !r.followups.is_empty() {
        s.push_str("\n\nFollow-up questions the user asked after reading the report:\n");
        for q in &r.followups {
            s.push_str(&format!("- {q}\n"));
        }
    }
    s.push_str(&context::block(&r.context));
    s
}

/// Appended to the instructions of the writers the user reads directly (synthesizer, chat).
fn style_suffix(style: &str) -> String {
    let s = style.trim();
    if s.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nThe user gave these instructions for how to write for them. Follow them for tone, audience, depth and format, but keep the citations:\n{s}"
        )
    }
}

impl Run {
    fn new(ctx: Ctx, record: Record) -> Arc<Run> {
        let n = record.agents.len();
        let status = record.status;
        let fetch = record.config.fetch_url;
        Arc::new(Run {
            ctx,
            http: tools::fetch_http(),
            state: Mutex::new(RunState {
                record,
                step: String::new(),
                stage: (status, Instant::now()),
                activity: vec![String::new(); n],
                planner_log: vec![],
                logs: vec![vec![]; n],
                synth_text: String::new(),
                final_report: None,
                reports: vec![None; n],
                error: None,
            }),
            cancel: CancellationToken::new(),
            server_compaction: AtomicBool::new(true),
            fetch_enabled: AtomicBool::new(fetch),
            reasoning_summary: AtomicBool::new(true),
            restarts: AtomicU32::new(0),
        })
    }

    /// New run: plans angles, then waits in `review`.
    /// `clarify` = false forces a plan on the first planner call (no questions).
    pub fn start(ctx: Ctx, question: String, cfg: RunConfig, context: Vec<LocalDoc>, clarify: bool) -> Arc<Run> {
        let mut record = Record::new(question, cfg);
        record.context = context;
        record.prompts = prompts::Prompts::load();
        let run = Run::new(ctx, record);
        run.persist();
        tokio::spawn(run.clone().plan_task(!clarify));
        run
    }

    /// `mem` carries in-memory agent reports from the live run; they are used before disk so a report whose write failed is not redone.
    pub fn resume(ctx: Ctx, record: Record, mem: Vec<Option<String>>) -> Arc<Run> {
        let status = record.status;
        let run = Run::new(ctx, record);
        match status {
            RunStatus::Review | RunStatus::Clarifying => {}
            RunStatus::GapReview => {
                run.with(|s| {
                    for i in 0..s.record.agents.len() {
                        if s.record.agents[i].status == AgentStatus::Done {
                            s.reports[i] = mem.get(i).cloned().flatten().or_else(|| s.record.read_agent_report(i));
                        }
                    }
                });
            }
            RunStatus::Done => {
                let (rep, reports) = run.with(|s| {
                    (
                        s.record.read_final(),
                        (0..s.record.agents.len()).map(|i| s.record.read_agent_report(i)).collect::<Vec<_>>(),
                    )
                });
                run.with(|s| {
                    s.final_report = rep;
                    s.reports = reports;
                });
            }
            _ => {
                let pending = run.with(|s| {
                    s.record.error = None;
                    if s.record.perspectives.is_empty() {
                        return None;
                    }
                    let mut pend = vec![];
                    for i in 0..s.record.agents.len() {
                        if s.record.agents[i].status == AgentStatus::Done {
                            s.reports[i] = mem.get(i).cloned().flatten().or_else(|| s.record.read_agent_report(i));
                            if s.reports[i].is_none() {
                                s.record.agents[i].status = AgentStatus::Pending;
                            }
                        } else {
                            s.record.agents[i].status = AgentStatus::Pending;
                            s.record.agents[i].error = None;
                            s.record.agents[i].turns = 0;
                        }
                        if s.record.agents[i].status != AgentStatus::Done {
                            pend.push(i);
                        }
                    }
                    s.record.status = RunStatus::Researching;
                    Some(pend)
                });
                run.persist();
                match pending {
                    None => {
                        tokio::spawn(run.clone().plan_task(false));
                    }
                    Some(p) => {
                        tokio::spawn(run.clone().drive(p));
                    }
                }
            }
        }
        run
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut RunState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    pub fn status(&self) -> RunStatus {
        self.with(|s| s.record.status)
    }

    fn notify(&self, ev: RunEvent) {
        let _ = self.ctx.tx.send(ev);
    }

    fn persist(&self) {
        let rec = self.with(|s| {
            s.record.recompute_costs(&self.ctx.pricing);
            s.record.clone()
        });
        if let Err(e) = rec.save() {
            self.notify(RunEvent::Toast(format!("Couldn't save research: {e}")));
        }
    }

    fn set_status(&self, st: RunStatus) {
        self.with(|s| s.record.status = st);
        self.persist();
    }

    fn add_sources(&self, agent: Option<usize>, found: Vec<(String, String)>, grounded: bool) {
        self.with(|s| sources::merge(&mut s.record.sources, found, agent, grounded));
    }

    fn add_usage(&self, scope: Scope, u: Usage) {
        self.with(|s| match scope {
            Scope::Planner => s.record.planner_usage += u,
            Scope::Compaction => s.record.compaction_usage += u,
            Scope::Agent(i) => s.record.agents[i].usage += u,
            Scope::Synth => s.record.synthesis.usage += u,
            Scope::Verify => s.record.verify_usage += u,
        });
        self.persist();
        let hit = self.with(|s| {
            let hit = s.record.status == RunStatus::Researching && !s.record.budget_hit && s.record.over_budget();
            s.record.budget_hit |= hit;
            hit
        });
        if hit {
            self.persist();
            self.notify(RunEvent::Toast("Run budget reached — finishing with the research so far".into()));
        }
    }

    fn set_step(&self, text: impl Into<String>) {
        let t = text.into();
        self.with(|s| s.step = t);
    }

    fn set_activity(&self, idx: usize, text: impl Into<String>) {
        let t = text.into();
        self.with(|s| {
            if let Some(a) = s.activity.get_mut(idx) {
                *a = t;
            }
        });
    }

    /// The angles the review screen edits: the proposed follow-ups during gap review, else the plan.
    pub fn review_angles(&self) -> Vec<Perspective> {
        self.with(|s| {
            if s.record.status == RunStatus::GapReview { s.record.gaps.clone() } else { s.record.perspectives.clone() }
        })
    }

    pub fn set_review_angles(&self, ps: Vec<Perspective>) {
        self.with(|s| {
            if s.record.status == RunStatus::GapReview {
                s.record.gaps = ps;
            } else {
                s.record.perspectives = ps;
            }
        });
        self.persist();
    }

    pub fn cancel(&self) {
        let st = self.status();
        if !st.is_live() && !matches!(st, RunStatus::Review | RunStatus::GapReview | RunStatus::Clarifying) {
            return;
        }
        self.with(|s| {
            s.record.status = RunStatus::Cancelled;
            for a in &mut s.record.agents {
                if a.status == AgentStatus::Running {
                    a.status = AgentStatus::Pending;
                }
            }
            s.activity.iter_mut().for_each(String::clear);
        });
        self.persist();
        self.cancel.cancel();
    }

    fn fail(&self, e: ApiError) {
        if e.kind == ErrKind::Cancelled {
            return;
        }
        self.with(|s| {
            s.record.status = RunStatus::Failed;
            s.record.error = Some(ErrRec::from(&e));
            s.error = Some(e.clone());
            for a in &mut s.record.agents {
                if a.status == AgentStatus::Running {
                    a.status = AgentStatus::Pending;
                }
            }
            s.activity.iter_mut().for_each(String::clear);
        });
        self.persist();
        self.cancel.cancel();
        self.notify(RunEvent::Failed(e));
    }

    /// From review: freeze the angles and start the researchers.
    pub fn launch(self: &Arc<Self>) {
        let n = self.with(|s| {
            let ps = s.record.perspectives.clone();
            s.record.agents = ps
                .iter()
                .map(|p| AgentRec {
                    title: p.title.clone(),
                    brief: p.brief.clone(),
                    status: AgentStatus::Pending,
                    error: None,
                    usage: Usage::default(),
                    cost_usd: None,
                    turns: 0,
                    followup: false,
                })
                .collect();
            s.activity = vec![String::new(); ps.len()];
            s.logs = vec![vec![]; ps.len()];
            s.record.sources.clear();
            s.reports = vec![None; ps.len()];
            s.record.status = RunStatus::Researching;
            ps.len()
        });
        self.persist();
        tokio::spawn(self.clone().drive((0..n).collect()));
    }

    // ---- API plumbing ----

    /// One API call with retry, mapping a rejected web_search into a clear message.
    async fn call(
        &self,
        req: &ResponseRequest,
        tx: Option<&mpsc::UnboundedSender<StreamDelta>>,
    ) -> Result<Completed, ApiError> {
        match self.call_once(req, tx).await {
            Err(e)
                if req.reasoning_summary
                    && e.kind == ErrKind::Fatal
                    && (e.code.as_deref() == Some("subscription_sharing_unsupported_capability")
                        || e.status == Some(400))
                    && err_param_has(&e, "reasoning") =>
            {
                self.reasoning_summary.store(false, Ordering::Relaxed);
                let mut r = req.clone();
                r.reasoning_summary = false;
                self.call_once(&r, tx).await
            }
            r => r,
        }
    }

    async fn call_once(
        &self,
        req: &ResponseRequest,
        tx: Option<&mpsc::UnboundedSender<StreamDelta>>,
    ) -> Result<Completed, ApiError> {
        match self.ctx.client.call_with_retry(req, tx, &self.cancel).await {
            Err(e)
                if e.kind == ErrKind::Fatal
                    && (e.code.as_deref() == Some("subscription_sharing_unsupported_capability")
                        || e.status == Some(400))
                    && err_param_has(&e, "web_search") =>
            {
                let mut m = ApiError::new(
                    ErrKind::Fatal,
                    format!("Web search unavailable for {} — choose another model", req.model),
                );
                m.status = e.status;
                m.code = e.code;
                m.param = e.param;
                m.request_id = e.request_id;
                Err(m)
            }
            r => r,
        }
    }

    fn forwarder(self: &Arc<Self>, target: Target) -> mpsc::UnboundedSender<StreamDelta> {
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamDelta>();
        let run = self.clone();
        tokio::spawn(async move {
            while let Some(d) = rx.recv().await {
                match (&target, d) {
                    (Target::Synth, StreamDelta::Text(t)) => run.with(|s| {
                        if s.synth_text.is_empty() {
                            s.step = "writing report".into();
                        }
                        s.synth_text.push_str(&t)
                    }),
                    (Target::Synth, StreamDelta::Restart) => run.with(|s| {
                        s.synth_text.clear();
                        s.step = "connection dropped — retrying".into();
                    }),
                    (Target::Synth, _) => {}
                    (t, StreamDelta::Reasoning(d)) => run.log(t, LogEntry::Thinking(d), true),
                    (t, StreamDelta::ReasoningBreak) => run.log(t, LogEntry::Thinking(String::new()), false),
                    (t @ Target::Agent(_), StreamDelta::Text(d)) => run.log(t, LogEntry::Text(d), true),
                    (t, StreamDelta::Restart) => {
                        run.log(t, LogEntry::Note("connection dropped — retrying".into()), false)
                    }
                    (t, StreamDelta::Item(it)) => {
                        let agent = if let Target::Agent(i) = t { Some(*i) } else { None };
                        match it["type"].as_str().unwrap_or("") {
                            "web_search_call" => {
                                let e = if it["action"]["type"] == "search" {
                                    LogEntry::Search(first_query(&it).unwrap_or_default())
                                } else if let Some(u) = it["action"]["url"].as_str() {
                                    LogEntry::Open(u.to_string())
                                } else {
                                    LogEntry::Note("web search".into())
                                };
                                run.log(t, e, false);
                                if let Some(i) = agent {
                                    let a = first_query(&it)
                                        .map(|q| format!("searching: {q}"))
                                        .unwrap_or_else(|| "searching".into());
                                    run.set_activity(i, a);
                                }
                            }
                            "function_call" => {
                                let args: Value =
                                    serde_json::from_str(it["arguments"].as_str().unwrap_or("")).unwrap_or(Value::Null);
                                let url = args["url"].as_str().unwrap_or("");
                                run.log(t, LogEntry::Open(url.to_string()), false);
                                if let Some(i) = agent {
                                    run.set_activity(i, format!("reading: {}", tools::host_of(url)));
                                }
                            }
                            "reasoning" => {
                                if let Some(i) = agent {
                                    run.set_activity(i, "thinking");
                                }
                            }
                            _ => {}
                        }
                    }
                    // planner text is the JSON plan
                    (Target::Planner, StreamDelta::Text(_)) => {}
                }
            }
        });
        tx
    }

    /// Add to a timeline; `merge` appends to a trailing entry of the same stream kind.
    fn log(&self, t: &Target, e: LogEntry, merge: bool) {
        self.with(|s| {
            if let Some(l) = s.log_for(t) {
                if merge { append_log(l, e) } else { push_log(l, e) }
            }
        });
    }

    fn note(&self, t: Target, text: impl Into<String>) {
        self.log(&t, LogEntry::Note(text.into()), false);
    }
}

/// `"What is RISC-V's future?"` → `what-is-risc-v-s-future`
pub fn slug(question: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in question.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let s: String = out.trim_matches('-').chars().take(50).collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "research".into() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_basic() {
        assert_eq!(slug("What is RISC-V's future?"), "what-is-risc-v-s-future");
        assert_eq!(slug("???"), "research");
        assert_eq!(slug(&"a".repeat(80)).len(), 50);
    }

    #[test]
    fn output_text_joins_message_parts() {
        let items = vec![
            json!({"type":"reasoning"}),
            json!({"type":"message","content":[{"type":"output_text","text":"a"},{"type":"output_text","text":"b"}]}),
        ];
        assert_eq!(output_text(&items), "ab");
    }

    #[test]
    fn append_log_merges_streams_and_caps() {
        let mut l = vec![];
        append_log(&mut l, LogEntry::Text("a".into()));
        append_log(&mut l, LogEntry::Text("b".into()));
        assert_eq!(l, vec![LogEntry::Text("ab".into())]);
        append_log(&mut l, LogEntry::Search("q".into()));
        append_log(&mut l, LogEntry::Text("c".into()));
        assert_eq!(l.len(), 3);
        let mut l = vec![];
        for i in 0..600 {
            push_log(&mut l, LogEntry::Note(i.to_string()));
        }
        assert_eq!(l.len(), 500);
        assert_eq!(l.last(), Some(&LogEntry::Note("599".into())));
    }
}
