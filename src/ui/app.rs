mod chat;
mod clarify;
mod history;
mod home;
mod modals;
mod review;
mod run;
mod settings;
mod signin;
mod viewer;

use super::fmt::{self, open_url};
use crate::api::{ApiError, Client, ErrKind, ModelInfo};
use crate::auth::{self, Account, AuthError, AuthHandle, SignInUi, USAGE_URL};
use crate::config::{Config, StageModel, resolve_model};
use crate::pricing::Pricing;
use crate::research::chat::Chat;
use crate::research::store::{ChatRole, LocalDoc, Perspective, Record, RunStatus, Source, Support};
use crate::research::{self, Ctx, Run, RunEvent};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType};
use ratatui_textarea::{TextArea, WrapMode};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

pub const MUTED: Color = Color::DarkGray;
pub const ACCENT: Color = Color::Cyan;

pub enum AppMsg {
    SignInUrl(String),
    SignIn(Result<Account, AuthError>),
    SignedOut(bool),
    Models(String, Result<Vec<ModelInfo>, ApiError>),
    UpdateAvailable(String),
    Context(Result<LocalDoc, String>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    SignIn,
    Home,
    Review,
    Clarify,
    Run,
    Viewer,
    History,
    Settings,
    Chat,
}

pub enum SignInPhase {
    Idle,
    Waiting { url: Option<String>, cancel: CancellationToken },
    Error(String),
    NotEnabled,
}

pub struct SignIn {
    pub phase: SignInPhase,
    pub existing: Option<Account>,
    /// Where Esc returns to when adding an account on top of a working session.
    pub return_to: Option<Screen>,
}

pub enum Models {
    Loading,
    Ready(Vec<ModelInfo>),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Planner,
    Researcher,
    Compactor,
    Synthesizer,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Planner => "Planner",
            Stage::Researcher => "Researcher",
            Stage::Compactor => "Compactor",
            Stage::Synthesizer => "Synthesizer",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickScope {
    Chip,
    Settings,
}

pub struct Picker {
    pub scope: PickScope,
    pub stage: Stage,
    pub effort_step: bool,
    pub chosen: Option<String>,
    pub sel: usize,
}

pub enum ConfirmKind {
    CancelReview,
    CancelRun,
    Delete(String),
    Overwrite { path: PathBuf, text: String },
    ExportPdf { path: PathBuf, rec: Box<Record>, report: String },
    Deepen { id: String, question: String, angles: Vec<Perspective> },
}

pub struct Confirm {
    pub kind: ConfirmKind,
    pub text: String,
}

pub struct SaveModal {
    pub ta: TextArea<'static>,
    pub text: String,
    pub err: Option<String>,
}

/// Export prompt: the record and report are captured when it opens.
pub struct ExportModal {
    pub ta: TextArea<'static>,
    pub err: Option<String>,
    pub rec: Record,
    pub report: String,
}

/// File path prompt (attach a local file).
pub struct PathInput {
    pub ta: TextArea<'static>,
    pub err: Option<String>,
}

pub struct AngleEdit {
    pub idx: Option<usize>,
    pub title: TextArea<'static>,
    pub brief: TextArea<'static>,
    pub focus_brief: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Num {
    DefaultAgents,
    MaxParallel,
    ReportWords,
}

pub struct NumberEdit {
    pub which: Num,
    pub ta: TextArea<'static>,
    pub err: Option<String>,
}

pub struct StyleEdit {
    pub ta: TextArea<'static>,
}

#[derive(Clone)]
pub enum AcctAction {
    Switch(String),
    Add,
    SignOut,
    Enable,
}

pub enum Modal {
    None,
    Welcome,
    UsageLimit,
    Picker(Picker),
    Angle(Box<AngleEdit>),
    Confirm(Confirm),
    Save(Box<SaveModal>),
    Account(Vec<(String, AcctAction)>, usize),
    Number(Box<NumberEdit>),
    Style(Box<StyleEdit>),
    Detail(Vec<String>),
    Export(Box<ExportModal>),
    Attach(Box<PathInput>),
    Help,
}

pub struct Home {
    pub input: TextArea<'static>,
    pub chips: bool,
    pub chip: usize,
    pub agents: u8,
    pub planner: StageModel,
    pub researcher: StageModel,
    pub synthesizer: StageModel,
    /// Preset chosen with the Preset chip; applied when the run starts.
    pub preset: Option<String>,
    /// Files attached with the Files chip.
    pub context: Vec<LocalDoc>,
}

pub struct ViewTab {
    pub label: String,
    pub text: String,
    pub rendered: Text<'static>,
    pub agent: Option<usize>,
    pub is_sources: bool,
}

pub struct Viewer {
    pub tabs: Vec<ViewTab>,
    pub tab: usize,
    pub scroll: u16,
    pub back: Screen,
    pub question: String,
    pub view_h: u16,
    pub max_scroll: u16,
    pub sources: Vec<Source>,
    pub src_sel: usize,
    /// Record and final report text; Some only when a Final tab exists.
    pub chat: Option<(Record, String)>,
}

pub struct History {
    pub items: Vec<Record>,
    pub filter: String,
    pub filtering: bool,
    pub sel: usize,
    /// Run id → lowercased final report, so the filter can match report text.
    pub reports: HashMap<String, String>,
    /// Kept across opens.
    pub sort: HistSort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistSort {
    Date,
    Cost,
}

impl HistSort {
    pub fn label(self) -> &'static str {
        match self {
            HistSort::Date => "date",
            HistSort::Cost => "cost",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Account,
    Usage,
    Model(Stage),
    Num(Num),
    FetchUrl,
    CheckLinks,
    VerifyCitations,
    GapRound,
    CheckUpdates,
    ReportStyle,
}

pub const ROWS: [Row; 15] = [
    Row::Account,
    Row::Usage,
    Row::Model(Stage::Planner),
    Row::Model(Stage::Researcher),
    Row::Model(Stage::Compactor),
    Row::Model(Stage::Synthesizer),
    Row::Num(Num::DefaultAgents),
    Row::Num(Num::MaxParallel),
    Row::Num(Num::ReportWords),
    Row::ReportStyle,
    Row::FetchUrl,
    Row::CheckLinks,
    Row::VerifyCitations,
    Row::GapRound,
    Row::CheckUpdates,
];

pub struct App {
    pub cfg: Config,
    pub cfg_err: Option<String>,
    pub pricing: Pricing,
    pub msg_tx: UnboundedSender<AppMsg>,
    pub run_tx: UnboundedSender<RunEvent>,
    pub account: Option<Account>,
    pub client: Option<Client>,
    pub screen: Screen,
    pub modal: Modal,
    pub signin: SignIn,
    pub models: Models,
    pub home: Home,
    pub run: Option<Arc<Run>>,
    pub review_sel: usize,
    pub run_sel: usize,
    pub viewer: Viewer,
    pub history: History,
    pub settings_sel: usize,
    pub toast: Option<(String, Instant)>,
    pub tick: usize,
    pub quit: bool,
    pub cwd: PathBuf,
    pub clarify: ClarifyForm,
    pub inspect: Inspect,
    pub update: Option<String>,
    pub chat: Option<ChatView>,
}

pub struct ClarifyForm {
    /// (run id, clarify round) the answers belong to.
    pub key: (String, u8),
    pub answers: Vec<TextArea<'static>>,
    pub focus: usize,
}

/// Run-screen inspector state; `scroll` counts lines up from the bottom (0 = follow latest).
#[derive(Default)]
pub struct Inspect {
    pub full: bool,
    pub scroll: usize,
    pub view_h: usize,
}

/// Follow-up chat screen state.
pub struct ChatView {
    pub chat: Arc<Chat>,
    pub input: TextArea<'static>,
    /// Lines up from the bottom; 0 = follow latest.
    pub scroll: usize,
    pub view_h: u16,
    rendered: Text<'static>,
    rendered_n: usize,
}

fn you_block(text: &str, out: &mut Vec<Line<'static>>) {
    out.push(Line::styled("You", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)));
    out.extend(text.lines().map(|l| Line::raw(l.to_string())));
    out.push(Line::raw(""));
}

impl ChatView {
    fn new(chat: Arc<Chat>) -> ChatView {
        let mut input = TextArea::default();
        input.set_placeholder_text("Ask a follow-up question");
        input.set_placeholder_style(Style::new().fg(MUTED));
        input.set_cursor_line_style(Style::default());
        input.set_wrap_mode(WrapMode::WordOrGlyph);
        ChatView { chat, input, scroll: 0, view_h: 10, rendered: Text::default(), rendered_n: 0 }
    }

    /// Conversation so far plus the answer in progress; empty when nothing has been asked.
    pub fn transcript(&mut self, tick: usize) -> Text<'static> {
        let (n, pending, stream, msgs) = self.chat.with(|s| {
            let n = s.log.messages.len();
            let msgs = (n != self.rendered_n).then(|| s.log.messages.clone());
            (n, s.pending.clone(), s.stream.clone(), msgs)
        });
        if let Some(msgs) = msgs {
            let mut lines: Vec<Line<'static>> = vec![];
            let last = msgs.len().saturating_sub(1);
            for (i, m) in msgs.into_iter().enumerate() {
                match m.role {
                    ChatRole::User => you_block(&m.text, &mut lines),
                    ChatRole::Assistant => {
                        lines.extend(own_text(tui_markdown::from_str(&m.text)).lines);
                        if i == last && !m.research.is_empty() {
                            lines.push(Line::styled("Proposed research — Ctrl+T to run", Style::new().fg(ACCENT)));
                            for (j, p) in m.research.iter().enumerate() {
                                lines.push(Line::styled(
                                    format!("  {}. {} — {}", j + 1, p.title, p.brief),
                                    Style::new().fg(MUTED),
                                ));
                            }
                        }
                        lines.push(Line::raw(""));
                    }
                }
            }
            self.rendered = Text::from(lines);
            self.rendered_n = n;
        }
        let mut text = self.rendered.clone();
        if let Some(q) = pending {
            you_block(&q, &mut text.lines);
            if stream.is_empty() {
                let s = fmt::SPINNER[tick % fmt::SPINNER.len()];
                text.lines.push(Line::styled(format!("{s} thinking…"), Style::new().fg(MUTED)));
            } else {
                text.lines.extend(stream.lines().map(|l| Line::raw(l.to_string())));
            }
        }
        text
    }
}

fn answer_ta() -> TextArea<'static> {
    let mut t = TextArea::default();
    t.set_placeholder_text("Your answer (optional)");
    t.set_placeholder_style(Style::new().fg(MUTED));
    t.set_cursor_line_style(Style::default());
    t.set_wrap_mode(WrapMode::WordOrGlyph);
    t
}

fn newline_key(k: &KeyEvent) -> bool {
    (k.code == KeyCode::Enter && k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)) || ctrl(k, 'j')
}

pub fn single_line_ta(text: &str) -> TextArea<'static> {
    let mut t = TextArea::new(vec![text.to_string()]);
    t.set_cursor_line_style(Style::default());
    t.move_cursor(ratatui_textarea::CursorMove::End);
    t
}

fn own_text(t: Text<'_>) -> Text<'static> {
    let lines: Vec<Line<'static>> = t
        .lines
        .into_iter()
        .map(|l| {
            let style = l.style;
            let spans: Vec<Span<'static>> =
                l.spans.into_iter().map(|s| Span::styled(s.content.into_owned(), s.style)).collect();
            Line::from(spans).style(style)
        })
        .collect();
    Text::from(lines)
}

fn ctrl(k: &KeyEvent, c: char) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char(c)
}

impl App {
    pub fn new(
        cfg: Config,
        cfg_err: Option<String>,
        pricing: Pricing,
        msg_tx: UnboundedSender<AppMsg>,
        run_tx: UnboundedSender<RunEvent>,
    ) -> App {
        let mut input = TextArea::default();
        input.set_placeholder_text("What do you want to research?");
        input.set_placeholder_style(Style::new().fg(MUTED));
        input.set_cursor_line_style(Style::default());
        input.set_wrap_mode(WrapMode::WordOrGlyph);
        input.set_block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(ACCENT)));
        let home = Home {
            input,
            chips: false,
            chip: 0,
            agents: cfg.default_agents,
            planner: cfg.planner.clone(),
            researcher: cfg.researcher.clone(),
            synthesizer: cfg.synthesizer.clone(),
            preset: None,
            context: vec![],
        };
        let mut app = App {
            cfg,
            cfg_err,
            pricing,
            msg_tx,
            run_tx,
            account: None,
            client: None,
            screen: Screen::SignIn,
            modal: Modal::None,
            signin: SignIn { phase: SignInPhase::Idle, existing: None, return_to: None },
            models: Models::Loading,
            home,
            run: None,
            review_sel: 0,
            run_sel: 0,
            viewer: Viewer {
                tabs: vec![],
                tab: 0,
                scroll: 0,
                back: Screen::Home,
                question: String::new(),
                view_h: 10,
                max_scroll: 0,
                sources: vec![],
                src_sel: 0,
                chat: None,
            },
            history: History {
                items: vec![],
                filter: String::new(),
                filtering: false,
                sel: 0,
                reports: HashMap::new(),
                sort: HistSort::Date,
            },
            settings_sel: 0,
            toast: None,
            tick: 0,
            quit: false,
            clarify: ClarifyForm { key: (String::new(), 0), answers: vec![], focus: 0 },
            inspect: Inspect::default(),
            update: None,
            chat: None,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        };
        app.check_update();
        app.route_startup();
        app
    }

    fn route_startup(&mut self) {
        let acct = auth::active_account_id().and_then(|id| Account::load(&id).ok());
        match acct {
            Some(a) if a.has_tokens() => {
                self.activate(a);
                self.screen = Screen::Home;
            }
            Some(a) => {
                self.signin.existing = Some(a);
                self.screen = Screen::SignIn;
            }
            None => self.screen = Screen::SignIn,
        }
    }

    fn check_update(&self) {
        if !self.cfg.check_updates {
            return;
        }
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            if let Ok(Some(tag)) = crate::update::check().await {
                let _ = tx.send(AppMsg::UpdateAvailable(tag));
            }
        });
    }

    pub fn toast(&mut self, s: impl Into<String>) {
        self.toast = Some((s.into(), Instant::now()));
    }

    pub fn ctx(&self) -> Option<Ctx> {
        Some(Ctx { client: self.client.clone()?, pricing: self.pricing.clone(), tx: self.run_tx.clone() })
    }

    pub fn catalog(&self) -> &[ModelInfo] {
        match &self.models {
            Models::Ready(v) => v,
            _ => &[],
        }
    }

    pub fn display_name(&self, slug: &str) -> String {
        self.catalog()
            .iter()
            .find(|m| m.slug == slug)
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| slug.to_string())
    }

    /// Stage with an empty/unknown model resolved against the catalog.
    pub fn resolved(&self, s: &StageModel) -> StageModel {
        let slugs: Vec<String> = self.catalog().iter().map(|m| m.slug.clone()).collect();
        if slugs.is_empty() { s.clone() } else { resolve_model(s, &slugs) }
    }

    pub fn current_stage(&self, scope: PickScope, st: Stage) -> StageModel {
        let s = match (scope, st) {
            (PickScope::Chip, Stage::Planner) => &self.home.planner,
            (PickScope::Chip, Stage::Researcher) => &self.home.researcher,
            (PickScope::Chip, Stage::Synthesizer) => &self.home.synthesizer,
            (_, Stage::Planner) => &self.cfg.planner,
            (_, Stage::Researcher) => &self.cfg.researcher,
            (_, Stage::Compactor) => &self.cfg.compactor,
            (_, Stage::Synthesizer) => &self.cfg.synthesizer,
        };
        self.resolved(s)
    }

    pub fn picker_entries(&self, p: &Picker) -> Vec<(String, bool)> {
        let cur = self.current_stage(p.scope, p.stage);
        if p.effort_step {
            let slug = p.chosen.clone().unwrap_or_default();
            let mut v = vec!["default".to_string()];
            if let Some(m) = self.catalog().iter().find(|m| m.slug == slug) {
                v.extend(m.efforts.iter().cloned());
            }
            v.into_iter()
                .map(|e| {
                    let c = e == cur.effort && slug == cur.model;
                    (e, c)
                })
                .collect()
        } else {
            self.catalog().iter().map(|m| (m.display_name.clone(), m.slug == cur.model)).collect()
        }
    }

    fn set_stage(&mut self, scope: PickScope, st: Stage, model: String, effort: String) {
        let sm = StageModel { model, effort };
        match st {
            Stage::Planner => {
                self.home.planner = sm.clone();
                if scope == PickScope::Settings {
                    self.cfg.planner = sm;
                }
            }
            Stage::Researcher => {
                self.home.researcher = sm.clone();
                if scope == PickScope::Settings {
                    self.cfg.researcher = sm;
                }
            }
            Stage::Synthesizer => {
                self.home.synthesizer = sm.clone();
                if scope == PickScope::Settings {
                    self.cfg.synthesizer = sm;
                }
            }
            Stage::Compactor => self.cfg.compactor = sm,
        }
        if scope == PickScope::Settings {
            self.save_cfg();
        }
    }

    fn save_cfg(&mut self) {
        self.cfg.clamp();
        if let Err(e) = self.cfg.save() {
            self.toast(format!("Couldn't save config: {e}"));
        }
    }

    // ---- accounts ----

    fn activate(&mut self, acct: Account) {
        let handle = AuthHandle::new(acct.id());
        self.client = Some(Client::new(handle));
        let needs_welcome = acct.plan_usage_enabled && !acct.welcomed;
        let enabled = acct.plan_usage_enabled;
        self.account = Some(acct);
        if !enabled {
            self.signin.phase = SignInPhase::NotEnabled;
            self.signin.existing = self.account.clone();
            self.screen = Screen::SignIn;
            return;
        }
        if needs_welcome {
            self.modal = Modal::Welcome;
        }
        self.fetch_models();
    }

    pub fn fetch_models(&mut self) {
        let (Some(c), Some(a)) = (self.client.clone(), self.account.as_ref()) else { return };
        self.models = Models::Loading;
        let id = a.id();
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let r = c.list_models().await;
            let _ = tx.send(AppMsg::Models(id, r));
        });
    }

    pub fn begin_sign_in(&mut self, existing: Option<Account>, reconsent: bool) {
        let cancel = CancellationToken::new();
        let (url_tx, mut url_rx) = unbounded_channel::<String>();
        let tx = self.msg_tx.clone();
        tokio::spawn({
            let tx = tx.clone();
            async move {
                while let Some(u) = url_rx.recv().await {
                    let _ = tx.send(AppMsg::SignInUrl(u));
                }
            }
        });
        let ui = SignInUi { url_tx, cancel: cancel.clone() };
        let ex = existing.clone();
        tokio::spawn(async move {
            let r = auth::sign_in(ex.as_ref(), reconsent, ui).await;
            let _ = tx.send(AppMsg::SignIn(r));
        });
        self.signin.existing = existing;
        self.signin.phase = SignInPhase::Waiting { url: None, cancel };
        self.screen = Screen::SignIn;
    }

    pub fn on_msg(&mut self, m: AppMsg) {
        match m {
            AppMsg::UpdateAvailable(tag) => self.update = Some(tag),
            AppMsg::Context(Ok(doc)) => {
                if self.home.context.len() >= research::context::MAX_FILES {
                    self.toast(format!("At most {} files", research::context::MAX_FILES));
                } else if self.home.context.iter().any(|d| d.path == doc.path) {
                    self.toast("Already attached");
                } else {
                    let note = if doc.truncated { " (truncated)" } else { "" };
                    self.toast(format!("Attached {}{note}", doc.name));
                    self.home.context.push(doc);
                }
            }
            AppMsg::Context(Err(e)) => self.toast(e),
            AppMsg::SignInUrl(u) => {
                if let SignInPhase::Waiting { url, .. } = &mut self.signin.phase {
                    *url = Some(u);
                }
            }
            AppMsg::SignIn(Ok(acct)) => {
                self.signin.phase = SignInPhase::Idle;
                self.signin.return_to = None;
                self.models = Models::Loading;
                self.activate(acct);
                if self.account.as_ref().is_some_and(|a| a.plan_usage_enabled) {
                    self.screen = Screen::Home;
                }
            }
            AppMsg::SignIn(Err(e)) => match e {
                AuthError::Cancelled => {
                    self.signin.phase = SignInPhase::Idle;
                    if let Some(s) = self.signin.return_to.take() {
                        self.screen = s;
                    }
                }
                AuthError::Declined => self.signin.phase = SignInPhase::NotEnabled,
                other => self.signin.phase = SignInPhase::Error(other.to_string()),
            },
            AppMsg::SignedOut(confirmed) => {
                if !confirmed {
                    self.toast("Signed out locally; remote revocation not confirmed — disconnect rsc in ChatGPT Settings if needed.");
                }
            }
            AppMsg::Models(id, r) => {
                if self.account.as_ref().map(|a| a.id()) != Some(id) {
                    return;
                }
                self.models = match r {
                    Ok(v) if v.is_empty() => Models::Failed("no models available".into()),
                    Ok(v) => Models::Ready(v),
                    Err(e) => {
                        if e.kind == ErrKind::Auth {
                            self.signin.existing = self.account.clone();
                            self.signin.phase = SignInPhase::Idle;
                            self.screen = Screen::SignIn;
                        }
                        Models::Failed(e.message)
                    }
                };
            }
        }
    }

    pub fn on_run_event(&mut self, ev: RunEvent) {
        match ev {
            RunEvent::PlanReady | RunEvent::GapsReady => {
                self.review_sel = 0;
                if self.screen == Screen::Run {
                    self.screen = Screen::Review;
                } else if matches!(ev, RunEvent::GapsReady) {
                    self.toast("Follow-up round proposed — Ctrl+G");
                } else {
                    self.toast("Plan ready — Ctrl+G");
                }
            }
            RunEvent::Done => {
                if matches!(self.screen, Screen::Run | Screen::Home) && matches!(self.modal, Modal::None) {
                    self.open_run_viewer(None, Screen::Home);
                } else {
                    self.toast("Research done — Ctrl+G");
                }
            }
            RunEvent::Failed(e) => {
                if e.kind == ErrKind::UsageLimit {
                    self.modal = Modal::UsageLimit;
                } else if self.screen != Screen::Run {
                    self.toast("Research failed — Ctrl+G");
                }
            }
            RunEvent::Toast(t) => self.toast(t),
            RunEvent::Questions => {
                if self.screen == Screen::Run && matches!(self.modal, Modal::None) {
                    self.open_clarify();
                } else {
                    self.toast("Planner has questions — Ctrl+G");
                }
            }
            RunEvent::ChatFailed { question, error } => {
                if let Some(cv) = &mut self.chat
                    && cv.input.lines().iter().all(|l| l.is_empty())
                {
                    cv.input.insert_str(&question);
                }
                if error.kind == ErrKind::UsageLimit {
                    self.modal = Modal::UsageLimit;
                } else {
                    self.toast(format!("Couldn't answer: {}", error.message));
                }
            }
        }
    }

    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if let Some((_, t)) = &self.toast
            && t.elapsed() > Duration::from_secs(5)
        {
            self.toast = None;
        }
    }

    pub fn run_live(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.status().is_live())
    }

    // ---- run lifecycle ----

    fn start_run(&mut self) {
        let q = self.home.input.lines().join("\n").trim().to_string();
        if q.is_empty() {
            return;
        }
        if self.run_live() {
            self.toast("A research is already running — Ctrl+G");
            return;
        }
        let Models::Ready(_) = self.models else {
            self.toast("Models not loaded yet");
            return;
        };
        let Some(ctx) = self.ctx() else { return };
        let mut rc = self.cfg.run_config(
            self.resolved(&self.home.planner),
            self.resolved(&self.home.researcher),
            self.resolved(&self.cfg.compactor),
            self.resolved(&self.home.synthesizer),
            self.home.agents,
        );
        if let Some(name) = self.home.preset.clone()
            && let Some(p) = self.cfg.presets.get(&name)
        {
            p.apply(&mut rc);
            rc.preset = name;
        }
        let context = std::mem::take(&mut self.home.context);
        self.run = Some(Run::start(ctx, q, rc, context, true));
        self.home.input.select_all();
        self.home.input.cut();
        self.run_sel = 0;
        self.inspect = Inspect::default();
        self.screen = Screen::Run;
    }

    fn goto_run(&mut self) {
        let Some(run) = self.run.clone() else {
            self.toast("No research in progress");
            return;
        };
        match run.status() {
            RunStatus::Review | RunStatus::GapReview => {
                self.review_sel = 0;
                self.screen = Screen::Review;
            }
            RunStatus::Clarifying => self.open_clarify(),
            RunStatus::Done => self.open_run_viewer(None, Screen::Home),
            _ => self.screen = Screen::Run,
        }
    }

    fn resume_from_history(&mut self, rec: Record) {
        let mem = match &self.run {
            Some(run) if run.with(|s| s.record.id == rec.id) => run.with(|s| s.reports.clone()),
            _ => vec![],
        };
        self.resume_record(rec, mem);
    }

    fn resume_record(&mut self, rec: Record, mem: Vec<Option<String>>) {
        if self.run_live() {
            self.toast("A research is already running — Ctrl+G");
            return;
        }
        let Some(ctx) = self.ctx() else { return };
        let status = rec.status;
        self.run = Some(Run::resume(ctx, rec, mem));
        self.run_sel = 0;
        self.review_sel = 0;
        self.inspect = Inspect::default();
        match status {
            RunStatus::Review | RunStatus::GapReview => self.screen = Screen::Review,
            RunStatus::Clarifying => self.open_clarify(),
            _ => self.screen = Screen::Run,
        }
    }

    // ---- key handling ----

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        if ctrl(&k, 'c') {
            self.quit = true;
            return;
        }
        if !matches!(self.modal, Modal::None) {
            let m = std::mem::replace(&mut self.modal, Modal::None);
            self.modal = self.modal_key(m, k);
            return;
        }
        if self.screen != Screen::SignIn {
            if ctrl(&k, 'r') {
                self.open_history();
                return;
            }
            if ctrl(&k, 'o') {
                self.screen = Screen::Settings;
                return;
            }
            if ctrl(&k, 'u') {
                open_url(USAGE_URL);
                return;
            }
            if ctrl(&k, 'g') {
                self.goto_run();
                return;
            }
            if k.code == KeyCode::F(1) || (k.code == KeyCode::Char('?') && self.help_key_free()) {
                self.modal = Modal::Help;
                return;
            }
        }
        match self.screen {
            Screen::SignIn => self.key_signin(k),
            Screen::Home => self.key_home(k),
            Screen::Review => self.key_review(k),
            Screen::Clarify => self.key_clarify(k),
            Screen::Run => self.key_run(k),
            Screen::Viewer => self.key_viewer(k),
            Screen::History => self.key_history(k),
            Screen::Settings => self.key_settings(k),
            Screen::Chat => self.key_chat(k),
        }
    }

    /// Whether `?` should open help (false where it is typed as text).
    fn help_key_free(&self) -> bool {
        match self.screen {
            Screen::Run | Screen::Review | Screen::Viewer | Screen::Settings => true,
            Screen::History => !self.history.filtering,
            Screen::Home => self.home.chips,
            Screen::Clarify | Screen::Chat | Screen::SignIn => false,
        }
    }

    pub fn on_paste(&mut self, s: String) {
        match &mut self.modal {
            Modal::Save(m) => {
                m.ta.insert_str(s.replace('\n', ""));
            }
            Modal::Export(m) => {
                m.ta.insert_str(s.replace('\n', ""));
            }
            Modal::Attach(m) => {
                m.ta.insert_str(s.replace('\n', ""));
            }
            Modal::Angle(a) => {
                let t = s.replace('\n', " ");
                if a.focus_brief {
                    a.brief.insert_str(t);
                } else {
                    a.title.insert_str(t);
                }
            }
            Modal::Number(n) => {
                n.ta.insert_str(s.replace('\n', ""));
            }
            Modal::Style(m) => {
                m.ta.insert_str(s);
            }
            Modal::None if self.screen == Screen::Home && !self.home.chips => {
                self.home.input.insert_str(s);
            }
            Modal::None if self.screen == Screen::Clarify => {
                let f = self.clarify.focus;
                if let Some(t) = self.clarify.answers.get_mut(f) {
                    t.insert_str(s);
                }
            }
            Modal::None if self.screen == Screen::Chat => {
                if let Some(c) = &mut self.chat {
                    c.input.insert_str(s);
                }
            }
            _ => {}
        }
    }
}
