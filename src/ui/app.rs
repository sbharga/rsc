use super::fmt::{self, expand_tilde, open_url, osc52_copy};
use crate::api::{ApiError, Client, ErrKind, ModelInfo};
use crate::auth::{self, Account, AuthError, AuthHandle, SignInUi, USAGE_URL};
use crate::config::{Config, StageModel, resolve_model};
use crate::pricing::Pricing;
use crate::research::store::{self, Perspective, Record, RunConfig, RunStatus, Source};
use crate::research::{self, Ctx, Run, RunEvent};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType};
use ratatui_textarea::{TextArea, WrapMode};
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
    Detail(Vec<String>),
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
    pub incognito: bool,
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
}

pub struct History {
    pub items: Vec<Record>,
    pub filter: String,
    pub filtering: bool,
    pub sel: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Account,
    Usage,
    Model(Stage),
    Num(Num),
    FetchUrl,
    Incognito,
}

pub const ROWS: [Row; 11] = [
    Row::Account,
    Row::Usage,
    Row::Model(Stage::Planner),
    Row::Model(Stage::Researcher),
    Row::Model(Stage::Compactor),
    Row::Model(Stage::Synthesizer),
    Row::Num(Num::DefaultAgents),
    Row::Num(Num::MaxParallel),
    Row::Num(Num::ReportWords),
    Row::FetchUrl,
    Row::Incognito,
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
            incognito: cfg.incognito_default,
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
            },
            history: History { items: vec![], filter: String::new(), filtering: false, sel: 0 },
            settings_sel: 0,
            toast: None,
            tick: 0,
            quit: false,
            clarify: ClarifyForm { key: (String::new(), 0), answers: vec![], focus: 0 },
            inspect: Inspect::default(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        };
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
            RunEvent::PlanReady => {
                self.review_sel = 0;
                if self.screen == Screen::Run {
                    self.screen = Screen::Review;
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
        let rc = RunConfig {
            planner: self.resolved(&self.home.planner),
            researcher: self.resolved(&self.home.researcher),
            compactor: self.resolved(&self.cfg.compactor),
            synthesizer: self.resolved(&self.home.synthesizer),
            agents: self.home.agents,
            max_parallel: self.cfg.max_parallel,
            fetch_url: self.cfg.fetch_url_enabled,
            incognito: self.home.incognito,
            report_words: self.cfg.report_words,
            max_turns: self.cfg.max_turns,
            compact_threshold_tokens: self.cfg.compact_threshold_tokens,
            synth_input_budget_tokens: self.cfg.synth_input_budget_tokens,
        };
        self.run = Some(Run::start(ctx, q, rc));
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
            RunStatus::Review => {
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
            RunStatus::Review => self.screen = Screen::Review,
            RunStatus::Clarifying => self.open_clarify(),
            _ => self.screen = Screen::Run,
        }
    }

    // ---- viewer ----

    fn make_tab(label: String, text: String, agent: Option<usize>) -> ViewTab {
        let rendered = own_text(tui_markdown::from_str(&text));
        ViewTab { label, text, rendered, agent, is_sources: false }
    }

    fn sources_tab(sources: &[Source]) -> ViewTab {
        ViewTab {
            label: format!("Sources ({})", sources.len()),
            text: format!("# Sources\n\n{}", research::sources::render_list(sources, |_| true)),
            rendered: Text::default(),
            agent: None,
            is_sources: true,
        }
    }

    fn open_viewer(
        &mut self,
        mut tabs: Vec<ViewTab>,
        tab: usize,
        back: Screen,
        question: String,
        sources: Vec<Source>,
    ) {
        if tabs.is_empty() {
            self.toast("Nothing to show yet");
            return;
        }
        if !sources.is_empty() {
            tabs.push(Self::sources_tab(&sources));
        }
        self.viewer = Viewer { tabs, tab, scroll: 0, back, question, view_h: 10, max_scroll: 0, sources, src_sel: 0 };
        self.screen = Screen::Viewer;
    }

    /// Viewer for the in-memory run; `agent` selects that agent's tab.
    pub fn open_run_viewer(&mut self, agent: Option<usize>, back: Screen) {
        let Some(run) = self.run.clone() else { return };
        let (q, fin, titles, reports, sources) = run.with(|s| {
            (
                s.record.question.clone(),
                s.final_report.clone(),
                s.record.agents.iter().map(|a| a.title.clone()).collect::<Vec<_>>(),
                s.reports.clone(),
                s.record.sources.clone(),
            )
        });
        let mut tabs = vec![];
        if let Some(f) = fin {
            tabs.push(Self::make_tab("Final".into(), f, None));
        }
        for (i, r) in reports.into_iter().enumerate() {
            if let Some(r) = r {
                tabs.push(Self::make_tab(titles[i].clone(), r, Some(i)));
            }
        }
        let sel = agent.and_then(|a| tabs.iter().position(|t| t.agent == Some(a))).unwrap_or(0);
        self.open_viewer(tabs, sel, back, q, sources);
    }

    fn open_record_viewer(&mut self, rec: &Record) {
        let mut tabs = vec![];
        if let Some(f) = rec.read_final() {
            tabs.push(Self::make_tab("Final".into(), f, None));
        }
        for (i, a) in rec.agents.iter().enumerate() {
            if let Some(r) = rec.read_agent_report(i) {
                tabs.push(Self::make_tab(a.title.clone(), r, Some(i)));
            }
        }
        self.open_viewer(tabs, 0, Screen::History, rec.question.clone(), rec.sources.clone());
    }

    fn open_save(&mut self) {
        let Some(t) = self.viewer.tabs.get(self.viewer.tab) else { return };
        let mut name = research::slug(&self.viewer.question);
        if t.is_sources {
            name.push_str("-sources");
        } else if let Some(i) = t.agent {
            name.push_str(&format!("-{}", i + 1));
        }
        let path = self.cwd.join(format!("{name}.md"));
        self.modal = Modal::Save(Box::new(SaveModal {
            ta: single_line_ta(&path.to_string_lossy()),
            text: t.text.clone(),
            err: None,
        }));
    }

    fn do_save(&mut self, path: PathBuf, text: &str) {
        match std::fs::write(&path, text) {
            Ok(()) => self.toast(format!("Saved to {}", path.display())),
            Err(e) => self.toast(format!("Save failed: {e}")),
        }
    }

    // ---- history ----

    pub fn open_history(&mut self) {
        let mut items = store::list();
        if let Some(run) = &self.run {
            let (id, rec) = run.with(|s| (s.record.id.clone(), s.record.clone()));
            if !rec.config.incognito {
                if let Some(slot) = items.iter_mut().find(|r| r.id == id) {
                    *slot = rec;
                }
            } else {
                items.insert(0, rec);
            }
        }
        self.history.items = items;
        self.history.sel = 0;
        self.history.filtering = false;
        self.history.filter.clear();
        self.screen = Screen::History;
    }

    pub fn history_visible(&self) -> Vec<&Record> {
        let f = self.history.filter.to_lowercase();
        self.history.items.iter().filter(|r| f.is_empty() || r.question.to_lowercase().contains(&f)).collect()
    }

    fn selected_history(&self) -> Option<Record> {
        self.history_visible().get(self.history.sel).map(|r| (*r).clone())
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
        }
    }

    /// Whether `?` should open help (false where it is typed as text).
    fn help_key_free(&self) -> bool {
        match self.screen {
            Screen::Run | Screen::Review | Screen::Viewer | Screen::Settings => true,
            Screen::History => !self.history.filtering,
            Screen::Home => self.home.chips,
            Screen::Clarify | Screen::SignIn => false,
        }
    }

    pub fn on_paste(&mut self, s: String) {
        match &mut self.modal {
            Modal::Save(m) => {
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
            Modal::None if self.screen == Screen::Home && !self.home.chips => {
                self.home.input.insert_str(s);
            }
            Modal::None if self.screen == Screen::Clarify => {
                let f = self.clarify.focus;
                if let Some(t) = self.clarify.answers.get_mut(f) {
                    t.insert_str(s);
                }
            }
            _ => {}
        }
    }

    fn key_signin(&mut self, k: KeyEvent) {
        let existing = self.signin.existing.clone();
        match &self.signin.phase {
            SignInPhase::Idle | SignInPhase::Error(_) => match k.code {
                KeyCode::Enter => self.begin_sign_in(existing, false),
                KeyCode::Esc => {
                    if let Some(s) = self.signin.return_to.take() {
                        self.screen = s;
                    } else {
                        self.quit = true;
                    }
                }
                _ => {}
            },
            SignInPhase::Waiting { url, cancel } => match k.code {
                KeyCode::Esc => cancel.cancel(),
                KeyCode::Char('c') => {
                    if let Some(u) = url {
                        osc52_copy(u);
                        self.toast("URL copied");
                    }
                }
                _ => {}
            },
            SignInPhase::NotEnabled => match k.code {
                KeyCode::Enter => self.begin_sign_in(existing, true),
                KeyCode::Esc => {
                    if let Some(s) = self.signin.return_to.take() {
                        self.signin.phase = SignInPhase::Idle;
                        self.screen = s;
                    } else {
                        self.quit = true;
                    }
                }
                _ => {}
            },
        }
    }

    fn key_home(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Tab {
            self.home.chips = !self.home.chips;
            return;
        }
        if !self.home.chips {
            let newline = newline_key(&k);
            if newline {
                self.home.input.insert_newline();
            } else if k.code == KeyCode::Enter {
                self.start_run();
            } else {
                self.home.input.input(k);
            }
            return;
        }
        match k.code {
            KeyCode::Left => self.home.chip = self.home.chip.saturating_sub(1),
            KeyCode::Right => self.home.chip = (self.home.chip + 1).min(4),
            KeyCode::Esc => self.home.chips = false,
            KeyCode::Char('+') | KeyCode::Char('=') if self.home.chip == 0 => {
                self.home.agents = (self.home.agents + 1).min(8)
            }
            KeyCode::Char('-') if self.home.chip == 0 => self.home.agents = self.home.agents.saturating_sub(1).max(1),
            KeyCode::Char('m') => open_url(USAGE_URL),
            KeyCode::Char('r') => {
                if matches!(self.models, Models::Failed(_)) {
                    self.fetch_models();
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => match self.home.chip {
                0 if k.code == KeyCode::Enter => {
                    self.home.agents = if self.home.agents >= 8 { 1 } else { self.home.agents + 1 }
                }
                1..=3 if k.code == KeyCode::Enter => {
                    let stage = [Stage::Planner, Stage::Researcher, Stage::Synthesizer][self.home.chip - 1];
                    self.modal = Modal::Picker(Picker {
                        scope: PickScope::Chip,
                        stage,
                        effort_step: false,
                        chosen: None,
                        sel: 0,
                    });
                    self.picker_preselect();
                }
                4 => self.home.incognito = !self.home.incognito,
                _ => {}
            },
            _ => {}
        }
    }

    fn picker_preselect(&mut self) {
        if let Modal::Picker(p) = &self.modal {
            let sel = self.picker_entries(p).iter().position(|(_, c)| *c).unwrap_or(0);
            if let Modal::Picker(p) = &mut self.modal {
                p.sel = sel;
            }
        }
    }

    fn key_review(&mut self, k: KeyEvent) {
        let Some(run) = self.run.clone() else { return };
        let n = run.with(|s| s.record.perspectives.len());
        match k.code {
            KeyCode::Up => self.review_sel = self.review_sel.saturating_sub(1),
            KeyCode::Down => self.review_sel = (self.review_sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('e') => {
                if let Some(p) = run.with(|s| s.record.perspectives.get(self.review_sel).cloned()) {
                    self.modal = Modal::Angle(Box::new(AngleEdit {
                        idx: Some(self.review_sel),
                        title: single_line_ta(&p.title),
                        brief: single_line_ta(&p.brief),
                        focus_brief: false,
                    }));
                }
            }
            KeyCode::Char('a') => {
                if n >= 8 {
                    self.toast("Maximum 8 angles");
                } else {
                    self.modal = Modal::Angle(Box::new(AngleEdit {
                        idx: None,
                        title: single_line_ta(""),
                        brief: single_line_ta(""),
                        focus_brief: false,
                    }));
                }
            }
            KeyCode::Char('d') => {
                if n <= 1 {
                    self.toast("At least one angle is required");
                } else {
                    let mut ps = run.with(|s| s.record.perspectives.clone());
                    ps.remove(self.review_sel);
                    run.set_perspectives(ps);
                    self.review_sel = self.review_sel.min(n - 2);
                }
            }
            KeyCode::Char('p') => {
                run.replan();
                self.screen = Screen::Run;
            }
            KeyCode::Enter => {
                run.launch();
                self.run_sel = 0;
                self.inspect = Inspect::default();
                self.screen = Screen::Run;
            }
            KeyCode::Esc => {
                self.modal = Modal::Confirm(Confirm {
                    kind: ConfirmKind::CancelReview,
                    text: "Cancel this research? y/n".into(),
                });
            }
            _ => {}
        }
    }

    fn key_run(&mut self, k: KeyEvent) {
        let Some(run) = self.run.clone() else {
            self.screen = Screen::Home;
            return;
        };
        let (n, status) = run.with(|s| (s.record.agents.len(), s.record.status));
        let page = self.inspect.view_h.max(1);
        match k.code {
            KeyCode::Up => {
                self.run_sel = self.run_sel.saturating_sub(1);
                self.inspect.scroll = 0;
            }
            KeyCode::Down => {
                self.run_sel = (self.run_sel + 1).min(n.saturating_sub(1));
                self.inspect.scroll = 0;
            }
            KeyCode::Char('i') if n > 0 => {
                self.inspect.full = !self.inspect.full;
                self.inspect.scroll = 0;
            }
            KeyCode::PageUp => self.inspect.scroll += page,
            KeyCode::PageDown => self.inspect.scroll = self.inspect.scroll.saturating_sub(page),
            KeyCode::End => self.inspect.scroll = 0,
            KeyCode::Enter => {
                let has = run.with(|s| s.reports.get(self.run_sel).is_some_and(|r| r.is_some()));
                if has {
                    self.open_run_viewer(Some(self.run_sel), Screen::Run);
                }
            }
            KeyCode::Char('x') if status.is_live() => {
                self.modal =
                    Modal::Confirm(Confirm { kind: ConfirmKind::CancelRun, text: "Cancel this research? y/n".into() });
            }
            KeyCode::Char('r') if status.is_resumable() => {
                let (rec, mem) = run.with(|s| (s.record.clone(), s.reports.clone()));
                self.resume_record(rec, mem);
            }
            KeyCode::Esc => {
                if self.inspect.full {
                    self.inspect.full = false;
                } else {
                    self.screen = Screen::Home;
                }
            }
            _ => {}
        }
    }

    fn open_clarify(&mut self) {
        let Some(run) = self.run.clone() else { return };
        let (key, n) = run.with(|s| ((s.record.id.clone(), s.record.clarify_rounds), s.record.questions.len()));
        if self.clarify.key != key || self.clarify.answers.len() != n {
            self.clarify = ClarifyForm { key, answers: (0..n).map(|_| answer_ta()).collect(), focus: 0 };
        }
        self.screen = Screen::Clarify;
    }

    fn key_clarify(&mut self, k: KeyEvent) {
        let Some(run) = self.run.clone() else {
            self.screen = Screen::Home;
            return;
        };
        if run.status() != RunStatus::Clarifying {
            self.goto_run();
            return;
        }
        let n = self.clarify.answers.len().max(1);
        let focus = self.clarify.focus.min(n - 1);
        match k.code {
            KeyCode::Tab => self.clarify.focus = (focus + 1) % n,
            KeyCode::BackTab => self.clarify.focus = (focus + n - 1) % n,
            KeyCode::Esc => {
                self.modal = Modal::Confirm(Confirm {
                    kind: ConfirmKind::CancelReview,
                    text: "Cancel this research? y/n".into(),
                });
            }
            _ if newline_key(&k) => {
                if let Some(t) = self.clarify.answers.get_mut(focus) {
                    t.insert_newline();
                }
            }
            KeyCode::Enter => {
                let answers = self.clarify.answers.iter().map(|t| t.lines().join("\n")).collect();
                run.answer(answers);
                self.run_sel = 0;
                self.inspect = Inspect::default();
                self.screen = Screen::Run;
            }
            _ => {
                if let Some(t) = self.clarify.answers.get_mut(focus) {
                    t.input(k);
                }
            }
        }
    }

    fn key_viewer(&mut self, k: KeyEvent) {
        if self.viewer.tabs.get(self.viewer.tab).is_some_and(|t| t.is_sources) && self.key_sources(k) {
            return;
        }
        let v = &mut self.viewer;
        let page = v.view_h.saturating_sub(1).max(1);
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => v.scroll = v.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => v.scroll = v.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => v.scroll = v.scroll.saturating_add(page),
            KeyCode::PageUp => v.scroll = v.scroll.saturating_sub(page),
            KeyCode::Char('g') => v.scroll = 0,
            KeyCode::Char('G') => v.scroll = u16::MAX,
            KeyCode::Tab => {
                v.tab = (v.tab + 1) % v.tabs.len();
                v.scroll = 0;
            }
            KeyCode::BackTab => {
                v.tab = (v.tab + v.tabs.len() - 1) % v.tabs.len();
                v.scroll = 0;
            }
            KeyCode::Char(c @ '1'..='9') => {
                let p = c as usize - '1' as usize;
                if p < v.tabs.len() {
                    v.tab = p;
                    v.scroll = 0;
                }
            }
            KeyCode::Char('s') => self.open_save(),
            KeyCode::Esc => {
                let back = self.viewer.back;
                if back == Screen::History {
                    self.open_history();
                } else {
                    self.screen = back;
                }
            }
            _ => {}
        }
    }

    /// Keys specific to the Sources tab; returns false to fall through to the generic viewer keys.
    fn key_sources(&mut self, k: KeyEvent) -> bool {
        let v = &mut self.viewer;
        let last = v.sources.len().saturating_sub(1);
        let page = (v.view_h as usize / 2).max(1);
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => v.src_sel = v.src_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => v.src_sel = (v.src_sel + 1).min(last),
            KeyCode::PageUp => v.src_sel = v.src_sel.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => v.src_sel = (v.src_sel + page).min(last),
            KeyCode::Char('g') => v.src_sel = 0,
            KeyCode::Char('G') => v.src_sel = last,
            KeyCode::Enter | KeyCode::Char('o') => {
                if let Some(s) = v.sources.get(v.src_sel) {
                    open_url(&s.url);
                }
            }
            KeyCode::Char('c') => {
                if let Some(s) = v.sources.get(v.src_sel) {
                    osc52_copy(&s.url);
                    self.toast("URL copied");
                }
            }
            _ => return false,
        }
        true
    }

    fn key_history(&mut self, k: KeyEvent) {
        if self.history.filtering {
            match k.code {
                KeyCode::Esc => {
                    self.history.filtering = false;
                    self.history.filter.clear();
                }
                KeyCode::Enter => self.history.filtering = false,
                KeyCode::Backspace => {
                    self.history.filter.pop();
                }
                KeyCode::Char(c) => self.history.filter.push(c),
                _ => {}
            }
            self.history.sel = 0;
            return;
        }
        let n = self.history_visible().len();
        match k.code {
            KeyCode::Up => self.history.sel = self.history.sel.saturating_sub(1),
            KeyCode::Down => self.history.sel = (self.history.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('/') => self.history.filtering = true,
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Enter => {
                let Some(rec) = self.selected_history() else { return };
                let active_id = self.run.as_ref().map(|r| r.with(|s| s.record.id.clone()));
                if active_id.as_deref() == Some(rec.id.as_str()) {
                    self.goto_run();
                    return;
                }
                match rec.status {
                    RunStatus::Done => self.open_record_viewer(&rec),
                    RunStatus::Review | RunStatus::Clarifying => self.resume_from_history(rec),
                    _ => {
                        let mut lines = vec![format!("{} · {}", rec.status.label(), fmt::one_line(&rec.question))];
                        if let Some(e) = &rec.error {
                            lines.push(e.message.clone());
                            if let Some(r) = &e.request_id {
                                lines.push(format!("request {r}"));
                            }
                        }
                        if rec.status.is_resumable() {
                            lines.push(String::new());
                            lines.push("r resume".into());
                        }
                        self.modal = Modal::Detail(lines);
                    }
                }
            }
            KeyCode::Char('r') => {
                if let Some(rec) = self.selected_history()
                    && rec.status.is_resumable()
                {
                    self.resume_from_history(rec);
                }
            }
            KeyCode::Char('d') => {
                if let Some(rec) = self.selected_history() {
                    let live =
                        self.run.as_ref().is_some_and(|r| r.status().is_live() && r.with(|s| s.record.id == rec.id));
                    if live {
                        self.toast("Can't delete the active research");
                    } else {
                        self.modal = Modal::Confirm(Confirm {
                            kind: ConfirmKind::Delete(rec.id),
                            text: "Delete this research? y/n".into(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    fn key_settings(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up => self.settings_sel = self.settings_sel.saturating_sub(1),
            KeyCode::Down => self.settings_sel = (self.settings_sel + 1).min(ROWS.len() - 1),
            KeyCode::Enter | KeyCode::Char(' ') => match ROWS[self.settings_sel] {
                Row::Account => {
                    let mut items: Vec<(String, AcctAction)> = vec![];
                    let active = self.account.as_ref().map(|a| a.id());
                    for a in auth::list_accounts() {
                        if Some(a.id()) != active {
                            items.push((format!("Switch to {} · {}", a.email, a.id()), AcctAction::Switch(a.id())));
                        }
                    }
                    items.push(("Add account".into(), AcctAction::Add));
                    if self.account.as_ref().is_some_and(|a| a.has_tokens()) {
                        items.push(("Sign out".into(), AcctAction::SignOut));
                    }
                    if self.account.as_ref().is_some_and(|a| !a.plan_usage_enabled) {
                        items.push(("Enable ChatGPT plan usage".into(), AcctAction::Enable));
                    }
                    self.modal = Modal::Account(items, 0);
                }
                Row::Usage => open_url(USAGE_URL),
                Row::Model(stage) if k.code == KeyCode::Enter => {
                    self.modal = Modal::Picker(Picker {
                        scope: PickScope::Settings,
                        stage,
                        effort_step: false,
                        chosen: None,
                        sel: 0,
                    });
                    self.picker_preselect();
                }
                Row::Model(_) => {}
                Row::Num(which) if k.code == KeyCode::Enter => {
                    let cur = match which {
                        Num::DefaultAgents => self.cfg.default_agents as u32,
                        Num::MaxParallel => self.cfg.max_parallel as u32,
                        Num::ReportWords => self.cfg.report_words,
                    };
                    self.modal =
                        Modal::Number(Box::new(NumberEdit { which, ta: single_line_ta(&cur.to_string()), err: None }));
                }
                Row::Num(_) => {}
                Row::FetchUrl => {
                    self.cfg.fetch_url_enabled = !self.cfg.fetch_url_enabled;
                    self.save_cfg();
                }
                Row::Incognito => {
                    self.cfg.incognito_default = !self.cfg.incognito_default;
                    self.home.incognito = self.cfg.incognito_default;
                    self.save_cfg();
                }
            },
            _ => {}
        }
    }

    // ---- modals ----

    fn modal_key(&mut self, m: Modal, k: KeyEvent) -> Modal {
        match m {
            Modal::None => Modal::None,
            Modal::Welcome => {
                if k.code == KeyCode::Enter {
                    if let Some(a) = &mut self.account {
                        a.welcomed = true;
                        let _ = a.save();
                    }
                    Modal::None
                } else {
                    Modal::Welcome
                }
            }
            Modal::UsageLimit => match k.code {
                KeyCode::Enter => {
                    open_url(USAGE_URL);
                    Modal::None
                }
                KeyCode::Esc => Modal::None,
                _ => Modal::UsageLimit,
            },
            Modal::Detail(lines) => match k.code {
                KeyCode::Esc | KeyCode::Enter => Modal::None,
                KeyCode::Char('r') => {
                    if let Some(rec) = self.selected_history()
                        && rec.status.is_resumable()
                    {
                        self.resume_from_history(rec);
                        return Modal::None;
                    }
                    Modal::Detail(lines)
                }
                _ => Modal::Detail(lines),
            },
            Modal::Picker(p) => self.picker_key(p, k),
            Modal::Angle(a) => self.angle_key(a, k),
            Modal::Confirm(c) => self.confirm_key(c, k),
            Modal::Save(s) => self.save_key(s, k),
            Modal::Number(n) => self.number_key(n, k),
            Modal::Account(items, sel) => self.account_key(items, sel, k),
            Modal::Help => Modal::None,
        }
    }

    fn picker_key(&mut self, mut p: Picker, k: KeyEvent) -> Modal {
        if !matches!(self.models, Models::Ready(_)) {
            match k.code {
                KeyCode::Esc => return Modal::None,
                KeyCode::Char('r') if matches!(self.models, Models::Failed(_)) => self.fetch_models(),
                _ => {}
            }
            return Modal::Picker(p);
        }
        let entries = self.picker_entries(&p);
        match k.code {
            KeyCode::Up => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down => p.sel = (p.sel + 1).min(entries.len().saturating_sub(1)),
            KeyCode::Esc => {
                if p.effort_step {
                    p.effort_step = false;
                    p.chosen = None;
                    let sel = self.picker_entries(&p).iter().position(|(_, c)| *c).unwrap_or(0);
                    p.sel = sel;
                } else {
                    return Modal::None;
                }
            }
            KeyCode::Enter => {
                if p.effort_step {
                    let effort = entries.get(p.sel).map(|e| e.0.clone()).unwrap_or_else(|| "default".into());
                    let model = p.chosen.clone().unwrap_or_default();
                    self.set_stage(p.scope, p.stage, model, effort);
                    return Modal::None;
                }
                let Some(m) = self.catalog().get(p.sel) else { return Modal::Picker(p) };
                p.chosen = Some(m.slug.clone());
                p.effort_step = true;
                let cur = self.current_stage(p.scope, p.stage);
                p.sel = self.picker_entries(&p).iter().position(|(e, _)| *e == cur.effort).unwrap_or(0);
            }
            _ => {}
        }
        Modal::Picker(p)
    }

    fn angle_key(&mut self, mut a: Box<AngleEdit>, k: KeyEvent) -> Modal {
        match k.code {
            KeyCode::Esc => return Modal::None,
            KeyCode::Tab | KeyCode::BackTab => a.focus_brief = !a.focus_brief,
            KeyCode::Enter => {
                let title = a.title.lines().join(" ").trim().to_string();
                let brief = a.brief.lines().join(" ").trim().to_string();
                if title.is_empty() {
                    a.focus_brief = false;
                    return Modal::Angle(a);
                }
                if let Some(run) = &self.run {
                    let mut ps = run.with(|s| s.record.perspectives.clone());
                    let p = Perspective { title, brief };
                    match a.idx {
                        Some(i) if i < ps.len() => ps[i] = p,
                        _ => {
                            ps.push(p);
                            self.review_sel = ps.len() - 1;
                        }
                    }
                    run.set_perspectives(ps);
                }
                return Modal::None;
            }
            _ => {
                if a.focus_brief {
                    a.brief.input(k);
                } else {
                    a.title.input(k);
                }
            }
        }
        Modal::Angle(a)
    }

    fn confirm_key(&mut self, c: Confirm, k: KeyEvent) -> Modal {
        match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                match c.kind {
                    ConfirmKind::CancelReview => {
                        if let Some(r) = &self.run {
                            r.cancel();
                        }
                        self.screen = Screen::Home;
                    }
                    ConfirmKind::CancelRun => {
                        if let Some(r) = &self.run {
                            r.cancel();
                        }
                    }
                    ConfirmKind::Delete(id) => {
                        let in_memory = self
                            .run
                            .as_ref()
                            .is_some_and(|r| r.with(|s| s.record.id == id && s.record.config.incognito));
                        let res = if in_memory { Ok(()) } else { store::delete(&id) };
                        match res {
                            Ok(()) => {
                                if self.run.as_ref().is_some_and(|r| r.with(|s| s.record.id == id)) {
                                    self.run = None;
                                }
                                self.history.items.retain(|r| r.id != id);
                                let n = self.history_visible().len();
                                self.history.sel = self.history.sel.min(n.saturating_sub(1));
                                self.toast("Deleted");
                            }
                            Err(e) => self.toast(format!("Delete failed: {e}")),
                        }
                    }
                    ConfirmKind::Overwrite { path, text } => self.do_save(path, &text),
                }
                Modal::None
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Modal::None,
            _ => Modal::Confirm(c),
        }
    }

    fn save_key(&mut self, mut s: Box<SaveModal>, k: KeyEvent) -> Modal {
        match k.code {
            KeyCode::Esc => Modal::None,
            KeyCode::Enter => {
                let raw = s.ta.lines().join("");
                let path = expand_tilde(raw.trim());
                if raw.trim().is_empty() {
                    return Modal::Save(s);
                }
                let parent =
                    path.parent().map(|p| if p.as_os_str().is_empty() { PathBuf::from(".") } else { p.to_path_buf() });
                if !parent.is_some_and(|p| p.is_dir()) {
                    s.err = Some("Folder doesn't exist".into());
                    return Modal::Save(s);
                }
                if path.exists() {
                    return Modal::Confirm(Confirm {
                        text: format!("Overwrite {}? y/n", path.display()),
                        kind: ConfirmKind::Overwrite { path, text: s.text },
                    });
                }
                self.do_save(path, &s.text);
                Modal::None
            }
            _ => {
                s.err = None;
                s.ta.input(k);
                Modal::Save(s)
            }
        }
    }

    fn number_key(&mut self, mut n: Box<NumberEdit>, k: KeyEvent) -> Modal {
        match k.code {
            KeyCode::Esc => Modal::None,
            KeyCode::Enter => {
                let txt = n.ta.lines().join("");
                let Ok(v) = txt.trim().parse::<u32>() else {
                    n.err = Some("Enter a number".into());
                    return Modal::Number(n);
                };
                match n.which {
                    Num::DefaultAgents => {
                        self.cfg.default_agents = v.clamp(1, 8) as u8;
                        self.home.agents = self.cfg.default_agents;
                    }
                    Num::MaxParallel => self.cfg.max_parallel = v.clamp(1, 8) as u8,
                    Num::ReportWords => self.cfg.report_words = v.clamp(100, 20_000),
                }
                self.save_cfg();
                Modal::None
            }
            _ => {
                n.err = None;
                if !matches!(k.code, KeyCode::Char(c) if !c.is_ascii_digit()) {
                    n.ta.input(k);
                }
                Modal::Number(n)
            }
        }
    }

    fn account_key(&mut self, items: Vec<(String, AcctAction)>, mut sel: usize, k: KeyEvent) -> Modal {
        match k.code {
            KeyCode::Esc => return Modal::None,
            KeyCode::Up => sel = sel.saturating_sub(1),
            KeyCode::Down => sel = (sel + 1).min(items.len().saturating_sub(1)),
            KeyCode::Enter => {
                match items[sel].1.clone() {
                    AcctAction::Add => {
                        self.signin.return_to = Some(Screen::Settings);
                        self.begin_sign_in(None, false);
                    }
                    AcctAction::Enable => {
                        self.signin.return_to = Some(Screen::Settings);
                        let ex = self.account.clone();
                        self.begin_sign_in(ex, true);
                    }
                    AcctAction::Switch(id) => {
                        if let Ok(a) = Account::load(&id) {
                            let _ = auth::set_active_account(Some(&id));
                            if let Some(r) = &self.run {
                                r.cancel();
                            }
                            if a.has_tokens() && a.plan_usage_enabled {
                                self.models = Models::Loading;
                                self.activate(a);
                            } else {
                                self.signin.return_to = Some(Screen::Settings);
                                self.begin_sign_in(Some(a), false);
                            }
                        }
                    }
                    AcctAction::SignOut => {
                        if let Some(a) = self.account.clone() {
                            if let Some(r) = &self.run {
                                r.cancel();
                            }
                            let tx = self.msg_tx.clone();
                            let id = a.id();
                            tokio::spawn(async move {
                                let ok = auth::sign_out(&id).await;
                                let _ = tx.send(AppMsg::SignedOut(ok));
                            });
                            let mut a = a;
                            a.access_token = None;
                            a.refresh_token = None;
                            a.id_token = None;
                            self.signin.existing = Some(a);
                            self.signin.phase = SignInPhase::Idle;
                            self.signin.return_to = None;
                            self.account = None;
                            self.client = None;
                            self.models = Models::Loading;
                            self.run = None;
                            self.screen = Screen::SignIn;
                        }
                    }
                }
                return Modal::None;
            }
            _ => {}
        }
        Modal::Account(items, sel)
    }
}
