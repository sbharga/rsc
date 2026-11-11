//! Research orchestration: planner → (review) → parallel researchers → synthesizer.

pub mod store;
pub mod tools;

use crate::api::{ApiError, Client, Completed, ErrKind, ResponseRequest, StreamDelta};
use crate::config::StageModel;
use crate::pricing::{Pricing, Usage};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use store::{AgentRec, AgentStatus, Clarification, ErrRec, Perspective, Record, RunConfig, RunStatus};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

/// Notifications that need the UI to act (everything else is read from the shared state each frame).
#[derive(Debug)]
pub enum RunEvent {
    PlanReady,
    Done,
    Failed(ApiError),
    Toast(String),
    Questions,
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
        (Some(LogEntry::Thinking(a)), LogEntry::Thinking(b)) | (Some(LogEntry::Text(a)), LogEntry::Text(b)) => a.push_str(b),
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
    /// In-memory agent reports (the only copy for incognito runs).
    pub reports: Vec<Option<String>>,
    pub error: Option<ApiError>,
    /// Live timelines (memory only; a resumed run starts empty).
    pub planner_log: Vec<LogEntry>,
    pub logs: Vec<Vec<LogEntry>>,
}

impl RunState {
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
}

enum Scope {
    Planner,
    Compaction,
    Agent(usize),
    Synth,
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

/// Plain-text rendering of input items for client-side compaction.
fn render_items(items: &[Value]) -> String {
    let mut out = String::new();
    for it in items {
        match it["type"].as_str().unwrap_or("") {
            "message" => {
                let role = it["role"].as_str().unwrap_or("");
                let text = match &it["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""),
                    _ => String::new(),
                };
                out.push_str(&format!("[{role}]\n{text}\n\n"));
            }
            "web_search_call" => {
                if let Some(q) = first_query(it) {
                    out.push_str(&format!("[search] {q}\n\n"));
                }
            }
            "function_call_output" => {
                let o = it["output"].as_str().unwrap_or("");
                out.push_str(&format!("[tool output]\n{}\n\n", tools::truncate_chars(o, 2000)));
            }
            _ => {}
        }
    }
    out
}

fn err_param_has(e: &ApiError, needle: &str) -> bool {
    e.param.as_deref().is_some_and(|p| p.contains(needle))
}

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
        .map(|a| a.iter().filter_map(|q| q.as_str()).map(|q| q.trim().to_string()).filter(|q| !q.is_empty()).take(3).collect())
        .unwrap_or_default();
    let ps: Vec<Perspective> = v["perspectives"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some(Perspective { title: p["title"].as_str()?.trim().to_string(), brief: p["brief"].as_str()?.trim().to_string() })
                })
                .take(8)
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
    fn new(ctx: Ctx, record: Record) -> Arc<Run> {
        let n = record.agents.len();
        let fetch = record.config.fetch_url;
        Arc::new(Run {
            ctx,
            http: tools::fetch_http(),
            state: Mutex::new(RunState {
                record,
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
        })
    }

    /// New run: plans angles, then waits in `review`.
    pub fn start(ctx: Ctx, question: String, cfg: RunConfig) -> Arc<Run> {
        let run = Run::new(ctx, Record::new(question, cfg));
        run.persist();
        tokio::spawn(run.clone().plan_task(false));
        run
    }

    /// `mem` carries in-memory agent reports (needed to resume incognito runs without redoing finished agents).
    pub fn resume(ctx: Ctx, record: Record, mem: Vec<Option<String>>) -> Arc<Run> {
        let status = record.status;
        let run = Run::new(ctx, record);
        match status {
            RunStatus::Review | RunStatus::Clarifying => {}
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
        f(&mut self.state.lock().unwrap())
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

    fn add_usage(&self, scope: Scope, u: Usage) {
        self.with(|s| match scope {
            Scope::Planner => s.record.planner_usage += u,
            Scope::Compaction => s.record.compaction_usage += u,
            Scope::Agent(i) => s.record.agents[i].usage += u,
            Scope::Synth => s.record.synthesis.usage += u,
        });
        self.persist();
    }

    fn set_activity(&self, idx: usize, text: impl Into<String>) {
        let t = text.into();
        self.with(|s| {
            if let Some(a) = s.activity.get_mut(idx) {
                *a = t;
            }
        });
    }

    pub fn set_perspectives(&self, ps: Vec<Perspective>) {
        self.with(|s| s.record.perspectives = ps);
        self.persist();
    }

    pub fn cancel(&self) {
        let st = self.status();
        if !st.is_live() && !matches!(st, RunStatus::Review | RunStatus::Clarifying) {
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

    /// Run the planner again (also used for the first plan).
    pub fn replan(self: &Arc<Self>) {
        tokio::spawn(self.clone().plan_task(true));
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
                })
                .collect();
            s.activity = vec![String::new(); ps.len()];
            s.logs = vec![vec![]; ps.len()];
            s.reports = vec![None; ps.len()];
            s.record.status = RunStatus::Researching;
            ps.len()
        });
        self.persist();
        tokio::spawn(self.clone().drive((0..n).collect()));
    }

    // ---- planner ----

    async fn plan_task(self: Arc<Self>, force: bool) {
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
                    let answer = if a.is_empty() { "(no answer — use your judgment)".to_string() } else { a.to_string() };
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
        let r = parse_plan(&text, force).ok_or_else(|| ApiError::new(ErrKind::Fatal, "Planner returned invalid plan"))?;
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

    // ---- API plumbing ----

    /// One API call with retry, mapping a rejected web_search into a clear message.
    async fn call(&self, req: &ResponseRequest, tx: Option<&mpsc::UnboundedSender<StreamDelta>>) -> Result<Completed, ApiError> {
        match self.call_once(req, tx).await {
            Err(e)
                if req.reasoning_summary
                    && e.kind == ErrKind::Fatal
                    && (e.code.as_deref() == Some("subscription_sharing_unsupported_capability") || e.status == Some(400))
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

    async fn call_once(&self, req: &ResponseRequest, tx: Option<&mpsc::UnboundedSender<StreamDelta>>) -> Result<Completed, ApiError> {
        match self.ctx.client.call_with_retry(req, tx, &self.cancel).await {
            Err(e)
                if e.kind == ErrKind::Fatal
                    && (e.code.as_deref() == Some("subscription_sharing_unsupported_capability") || e.status == Some(400))
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
                    (Target::Synth, StreamDelta::Text(t)) => run.with(|s| s.synth_text.push_str(&t)),
                    (Target::Synth, StreamDelta::Restart) => run.with(|s| s.synth_text.clear()),
                    (Target::Synth, _) => {}
                    (t, StreamDelta::Reasoning(d)) => run.log(t, LogEntry::Thinking(d), true),
                    (t, StreamDelta::ReasoningBreak) => run.log(t, LogEntry::Thinking(String::new()), false),
                    (t @ Target::Agent(_), StreamDelta::Text(d)) => run.log(t, LogEntry::Text(d), true),
                    (t, StreamDelta::Restart) => run.log(t, LogEntry::Note("connection dropped — retrying".into()), false),
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
                                    let a = first_query(&it).map(|q| format!("searching: {q}")).unwrap_or_else(|| "searching".into());
                                    run.set_activity(i, a);
                                }
                            }
                            "function_call" => {
                                let args: Value = serde_json::from_str(it["arguments"].as_str().unwrap_or("")).unwrap_or(Value::Null);
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

    fn agent_tools(&self, final_turn: bool) -> Vec<Value> {
        if final_turn {
            return vec![];
        }
        let mut t = vec![tools::web_search_tool()];
        if self.fetch_enabled.load(Ordering::Relaxed) {
            t.push(tools::fetch_url_namespace());
        }
        t
    }

    /// Researcher call with the documented fallbacks (server compaction, namespace tools).
    async fn agent_call(
        &self,
        req: &mut ResponseRequest,
        tx: Option<&mpsc::UnboundedSender<StreamDelta>>,
    ) -> Result<Completed, ApiError> {
        loop {
            match self.call(req, tx).await {
                Err(e) if e.kind == ErrKind::Fatal && req.context_management.is_some()
                    && (e.code.as_deref() == Some("subscription_sharing_unsupported_capability") || e.status == Some(400))
                    && err_param_has(&e, "context_management") =>
                {
                    self.server_compaction.store(false, Ordering::Relaxed);
                    req.context_management = None;
                }
                Err(e) if e.kind == ErrKind::Fatal
                    && self.fetch_enabled.load(Ordering::Relaxed)
                    && req.tools.iter().any(|t| t["type"] == "namespace")
                    && e.status == Some(400)
                    && (err_param_has(&e, "namespace") || err_param_has(&e, "fetch_url") || err_param_has(&e, "tools[1]")) =>
                {
                    self.fetch_enabled.store(false, Ordering::Relaxed);
                    req.tools.retain(|t| t["type"] != "namespace");
                    self.notify(RunEvent::Toast("fetch_url unsupported on this route".into()));
                }
                r => return r,
            }
        }
    }

    async fn client_compact(&self, input: &[Value]) -> Result<String, ApiError> {
        let sm = self.with(|s| s.record.config.compactor.clone());
        let done = self
            .call(
                &ResponseRequest {
                    model: sm.model,
                    instructions: "Summarize this research session so it can be continued: keep findings, evidence, source URLs, and what remains to check.".into(),
                    input: vec![user_msg(&render_items(input))],
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

    // ---- researchers ----

    async fn drive(self: Arc<Self>, run_agents: Vec<usize>) {
        self.with(|s| s.record.status = RunStatus::Researching);
        self.persist();
        let sem = Arc::new(Semaphore::new(self.with(|s| s.record.config.max_parallel.max(1)) as usize));
        let mut set = tokio::task::JoinSet::new();
        for idx in run_agents {
            let run = self.clone();
            let sem = sem.clone();
            set.spawn(async move {
                let _permit = tokio::select! {
                    _ = run.cancel.cancelled() => return,
                    p = sem.acquire_owned() => match p { Ok(p) => p, Err(_) => return },
                };
                run.with(|s| s.record.agents[idx].status = AgentStatus::Running);
                run.note(Target::Agent(idx), "started");
                run.persist();
                match run.run_agent(idx).await {
                    Ok(text) => {
                        let rec = run.with(|s| {
                            s.reports[idx] = Some(text.clone());
                            s.record.agents[idx].status = AgentStatus::Done;
                            s.record.agents[idx].error = None;
                            s.record.clone()
                        });
                        if let Err(e) = rec.write_agent_report(idx, &text) {
                            run.notify(RunEvent::Toast(format!("Couldn't save report: {e}")));
                        }
                        run.set_activity(idx, "");
                        run.note(Target::Agent(idx), "report ready — Enter to read");
                        run.persist();
                    }
                    Err(e) if e.kind == ErrKind::Cancelled => {}
                    Err(e) if e.kind == ErrKind::UsageLimit => run.fail(e),
                    Err(e) => {
                        run.note(Target::Agent(idx), format!("failed: {}", e.message));
                        run.with(|s| {
                            s.record.agents[idx].status = AgentStatus::Failed;
                            s.record.agents[idx].error = Some(e.message.clone());
                        });
                        run.set_activity(idx, "");
                        run.persist();
                    }
                }
            });
        }
        while set.join_next().await.is_some() {}
        if self.cancel.is_cancelled() {
            return;
        }
        let ok = self.with(|s| s.record.agents.iter().filter(|a| a.status == AgentStatus::Done).count());
        if ok == 0 {
            let msg = self.with(|s| s.record.agents.iter().find_map(|a| a.error.clone()));
            self.fail(ApiError::new(
                ErrKind::Fatal,
                format!("All researchers failed{}", msg.map(|m| format!(": {m}")).unwrap_or_default()),
            ));
            return;
        }
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

    async fn run_agent(self: &Arc<Self>, idx: usize) -> Result<String, ApiError> {
        let (brief, p, cfg) =
            self.with(|s| (task_brief(&s.record), s.record.perspectives[idx].clone(), s.record.config.clone()));
        let sm: &StageModel = &cfg.researcher;
        let mut req = ResponseRequest {
            model: sm.model.clone(),
            instructions: format!(
                "You are one of several researchers working independently on the problem below. Your angle: {} — {}\nInvestigate as you see fit using the available tools. When finished, reply with a Markdown report of at most about {} words: key findings, evidence, open uncertainties, and the source URLs you relied on.",
                p.title, p.brief, cfg.report_words
            ),
            input: vec![user_msg(&brief)],
            tools: vec![],
            effort: Some(sm.effort.clone()),
            text: None,
            context_management: None,
            reasoning_summary: false,
        };
        let tx = self.forwarder(Target::Agent(idx));
        let mut turns = 0u32;
        loop {
            let final_turn = turns >= cfg.max_turns;
            if final_turn {
                self.note(Target::Agent(idx), "turn limit reached — writing report");
                req.input.push(user_msg("Stop researching now and write your report."));
            }
            req.tools = self.agent_tools(final_turn);
            req.reasoning_summary = self.reasoning_summary.load(Ordering::Relaxed);
            req.context_management = (!final_turn && self.server_compaction.load(Ordering::Relaxed))
                .then(|| vec![json!({"type":"compaction","compact_threshold":cfg.compact_threshold_tokens})]);
            self.set_activity(idx, "thinking");
            let done = self.agent_call(&mut req, Some(&tx)).await?;
            turns += 1;
            self.with(|s| s.record.agents[idx].turns = turns);
            self.add_usage(Scope::Agent(idx), done.usage);

            if done.output.iter().any(|i| i["type"] == "compaction") {
                req.input.truncate(1);
            }
            req.input.extend(done.output.iter().cloned());

            let calls: Vec<&Value> = done.output.iter().filter(|i| i["type"] == "function_call").collect();
            if final_turn || calls.is_empty() {
                let t = output_text(&done.output);
                return Ok(if t.trim().is_empty() { "(no report)".into() } else { t });
            }
            for c in calls {
                let args = c["arguments"].as_str().unwrap_or("");
                let name = c["name"].as_str().unwrap_or("");
                if name == "fetch_url" {
                    let v: Value = serde_json::from_str(args).unwrap_or(Value::Null);
                    self.set_activity(idx, format!("reading: {}", tools::host_of(v["url"].as_str().unwrap_or(""))));
                }
                let out = tokio::select! {
                    _ = self.cancel.cancelled() => return Err(ApiError::cancelled()),
                    o = tools::execute(&self.http, name, args) => o,
                };
                req.input.push(json!({"type":"function_call_output","call_id":c["call_id"],"output":out}));
            }
            if !self.server_compaction.load(Ordering::Relaxed) && done.usage.input + done.usage.output > cfg.compact_threshold_tokens {
                self.set_activity(idx, "compacting");
                self.note(Target::Agent(idx), "compacting context");
                let summary = self.client_compact(&req.input).await?;
                req.input = vec![user_msg(&brief), user_msg(&format!("Notes so far:\n{summary}\nContinue."))];
            }
        }
    }

    // ---- synthesis ----

    async fn synthesize(self: &Arc<Self>) -> Result<String, ApiError> {
        let (brief, cfg, agents, mem_reports) =
            self.with(|s| (task_brief(&s.record), s.record.config.clone(), s.record.agents.clone(), s.reports.clone()));
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
        let input = format!("Problem:\n{brief}\n\n{}", parts.concat());
        let sm = &cfg.synthesizer;
        let req = ResponseRequest {
            model: sm.model.clone(),
            instructions: "Several researchers investigated the problem below from different angles. Combine their reports into one final answer in Markdown: integrate findings, resolve or flag conflicts, and cite sources inline. You may search the web to verify key claims.".into(),
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
        Ok(if text.trim().is_empty() { self.with(|s| s.synth_text.clone()) } else { text })
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
    fn render_skips_reasoning_and_truncates_outputs() {
        let items = vec![
            user_msg("q"),
            json!({"type":"reasoning","encrypted_content":"x"}),
            json!({"type":"web_search_call","action":{"type":"search","query":"foo"}}),
            json!({"type":"function_call_output","output":"y".repeat(5000)}),
        ];
        let r = render_items(&items);
        assert!(r.contains("[search] foo") && r.contains("[truncated]") && !r.contains("encrypted"));
    }

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
