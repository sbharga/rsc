//! Headless commands: `rsc run`, `rsc resume`, `rsc ls`, `rsc show`. No ratatui.
//!
//! Never calls `store::mark_interrupted`: that would flip a run that a TUI in another process is still working on.

use crate::api::Client;
use crate::auth::{self, Account, AuthHandle};
use crate::config::{self, Config, MAX_AGENTS};
use crate::export::{self, Format};
use crate::paths;
use crate::pricing::Pricing;
use crate::research::store::{self, AgentStatus, LinkStatus, RunStatus, Support};
use crate::research::{Ctx, Run, RunEvent, plan};
use crate::research::{context, prompts};
use crate::term;
use crate::ui::fmt;
use anyhow::{Context, bail};
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader, Lines, Stdin};
#[cfg(unix)]
use tokio::signal::unix::{Signal as Interrupt, SignalKind, signal};
#[cfg(windows)]
use tokio::signal::windows::CtrlC as Interrupt;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

/// Wrong arguments; `main` prints `usage()` for it.
#[derive(Debug)]
pub struct UsageError;

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Invalid arguments.")
    }
}

impl std::error::Error for UsageError {}

/// (command, arguments, description)
const COMMANDS: &[(&str, &str, &str)] = &[
    ("rsc", "", "Open the app."),
    ("rsc --version", "", "Print the version."),
    ("rsc update", "", "Install the newest release."),
    ("rsc uninstall", "[--purge | --keep-data] [--yes]", "Remove rsc; --purge also deletes ~/.rsc."),
    (
        "rsc run",
        "\"<question>\" [--agents N|auto] [--preset NAME] [--context FILE]... [--verify] [--yes] [--no-clarify] [-o FILE] [--json]",
        "Research a question without the app.",
    ),
    (
        "rsc resume",
        "<id> [--yes] [--no-clarify] [-o FILE] [--json]",
        "Continue a failed, cancelled or interrupted run.",
    ),
    ("rsc ls", "", "List research."),
    ("rsc show", "<id>", "Print a report."),
    ("rsc export", "<id> [--format md|html|json|pdf] [-o FILE]", "Export a report."),
];

pub fn usage() -> String {
    let p = term::err();
    let mut s = p.bold("Usage:");
    for (cmd, args, desc) in COMMANDS {
        s.push_str("\n  ");
        s.push_str(&p.bold(cmd));
        if !args.is_empty() {
            s.push(' ');
            s.push_str(args);
        }
        s.push_str("\n      ");
        s.push_str(&p.dim(desc));
    }
    s
}

/// Status text colored by outcome: done green, failed red, cancelled/interrupted yellow, in progress cyan.
fn paint_status(p: term::Paint, s: RunStatus, text: &str) -> String {
    match s {
        RunStatus::Done => p.green(text),
        RunStatus::Failed => p.red(text),
        RunStatus::Cancelled | RunStatus::Interrupted => p.yellow(text),
        _ => p.cyan(text),
    }
}

/// Ctrl+C: SIGINT on Unix, the console Ctrl+C event on Windows.
#[cfg(unix)]
fn interrupt() -> std::io::Result<Interrupt> {
    signal(SignalKind::interrupt())
}

/// Ctrl+C: SIGINT on Unix, the console Ctrl+C event on Windows.
#[cfg(windows)]
fn interrupt() -> std::io::Result<Interrupt> {
    tokio::signal::windows::ctrl_c()
}

const NEED_YES: &str = "Pass --yes to accept the plan when not running in a terminal.";

#[derive(Debug, Default, PartialEq)]
struct Opts {
    /// Question for `run`, id for `resume`, `show` and `export`; empty for `ls`.
    target: String,
    /// `Some(0)` = `--agents auto`.
    agents: Option<u8>,
    verify: bool,
    yes: bool,
    no_clarify: bool,
    output: Option<PathBuf>,
    json: bool,
    format: Option<Format>,
    preset: Option<String>,
    context: Vec<PathBuf>,
}

fn parse_opts(cmd: &str, args: &[String]) -> anyhow::Result<Opts> {
    let runs = matches!(cmd, "run" | "resume");
    let mut o = Opts::default();
    let mut positional = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--agents" if cmd == "run" => {
                let v = match it.next().map(String::as_str) {
                    Some("auto") => Some(0),
                    v => v.and_then(|v| v.parse::<u8>().ok()).filter(|n| (1..=MAX_AGENTS).contains(n)),
                };
                o.agents = Some(v.with_context(|| format!("--agents must be 1–{MAX_AGENTS} or auto."))?);
            }
            "--agents" if cmd == "resume" => bail!("--agents only applies to rsc run."),
            "--verify" if cmd == "run" => o.verify = true,
            "--verify" if cmd == "resume" => bail!("--verify only applies to rsc run."),
            "--yes" | "-y" if runs => o.yes = true,
            "--no-clarify" if runs => o.no_clarify = true,
            "--json" if runs => o.json = true,
            "-o" | "--output" if runs || cmd == "export" => {
                o.output = Some(PathBuf::from(it.next().with_context(|| format!("{a} needs a file."))?));
            }
            "--format" if cmd == "export" => {
                let v = it.next().map(String::as_str).and_then(Format::parse);
                o.format = Some(v.context("--format must be md, html, json or pdf.")?);
            }
            "--preset" if cmd == "run" => {
                o.preset = Some(it.next().context("--preset needs a name.")?.clone());
            }
            "--preset" if cmd == "resume" => bail!("--preset only applies to rsc run."),
            "--context" if cmd == "run" => {
                o.context.push(PathBuf::from(it.next().context("--context needs a file.")?));
            }
            "--context" if cmd == "resume" => bail!("--context only applies to rsc run."),
            x if x.starts_with('-') => bail!("Unknown option {x}."),
            _ => positional.push(a.clone()),
        }
    }
    match (cmd, positional.len()) {
        ("ls", 0) => {}
        ("run" | "resume" | "show" | "export", 1) => o.target = positional.remove(0),
        _ => return Err(UsageError.into()),
    }
    if cmd == "run" && o.target.trim().is_empty() {
        bail!("Question is empty.");
    }
    if o.context.len() > context::MAX_FILES {
        bail!("At most {} context files.", context::MAX_FILES);
    }
    Ok(o)
}

pub async fn main(args: &[String]) -> anyhow::Result<i32> {
    let cmd = args.first().map(String::as_str).unwrap_or_default();
    let opts = parse_opts(cmd, &args[1..])?;
    match cmd {
        "ls" => ls(),
        "show" => show(&opts.target),
        "export" => export(opts).await,
        "run" => run(opts).await,
        _ => resume(opts).await,
    }
}

fn ls() -> anyhow::Result<i32> {
    let p = term::out();
    for r in store::list() {
        let status = format!("{:<12}", r.status.label());
        println!(
            "{}  {}  {}  {}",
            p.cyan(&r.id),
            paint_status(p, r.status, &status),
            p.dim(fmt::badge(&r)),
            fmt::trunc(&fmt::one_line(&r.question), 80)
        );
    }
    Ok(0)
}

fn show(id: &str) -> anyhow::Result<i32> {
    let rec = store::load(id)?;
    let Some(report) = rec.read_final() else {
        bail!("No report for {id} (status: {}).", rec.status.label());
    };
    print_report(&report);
    Ok(0)
}

async fn export(opts: Opts) -> anyhow::Result<i32> {
    let id = &opts.target;
    let rec = store::load(id)?;
    let Some(report) = rec.read_final() else {
        bail!("No report for {id} (status: {}).", rec.status.label());
    };
    let format = opts.format.or_else(|| opts.output.as_deref().and_then(Format::from_path)).unwrap_or(Format::Md);
    match (&opts.output, format) {
        (None, Format::Pdf) => bail!("PDF export needs -o FILE."),
        (Some(path), Format::Pdf) => export::pdf(&rec, &report, path).await?,
        (None, f) => print_report(&export::render(&rec, &report, f)),
        (Some(path), f) => {
            paths::atomic_write(path, export::render(&rec, &report, f).as_bytes())
                .with_context(|| format!("Couldn't write {}.", path.display()))?;
        }
    }
    if let Some(path) = &opts.output {
        term::note(format!("exported to {}", term::err().bold(path.display())));
    }
    Ok(0)
}

fn print_report(report: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(report.as_bytes());
    if !report.ends_with('\n') {
        let _ = out.write_all(b"\n");
    }
    let _ = out.flush();
}

struct Setup {
    ctx: Ctx,
    client: Client,
    cfg: Config,
    rx: UnboundedReceiver<RunEvent>,
    tty: bool,
}

fn setup() -> anyhow::Result<Setup> {
    paths::ensure_dirs()?;
    let (cfg, banner) = Config::load();
    if let Some(b) = banner {
        term::warn(b);
    }
    let pricing = Pricing::load();
    prompts::seed();
    let tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let acct = auth::active_account_id().and_then(|id| Account::load(&id).ok()).filter(Account::has_tokens);
    let Some(acct) = acct else { bail!("Not signed in. Run rsc to sign in.") };
    if !acct.plan_usage_enabled {
        bail!("ChatGPT plan usage is not enabled. Run rsc to enable it.");
    }
    let client = Client::new(AuthHandle::new(acct.id()));
    let (tx, rx) = unbounded_channel();
    Ok(Setup { ctx: Ctx { client: client.clone(), pricing, tx }, client, cfg, rx, tty })
}

async fn run(opts: Opts) -> anyhow::Result<i32> {
    let s = setup()?;
    let preset = match &opts.preset {
        Some(name) => match s.cfg.presets.get(name) {
            Some(p) => Some((name.clone(), p.clone())),
            None => {
                let names: Vec<_> = s.cfg.presets.keys().map(String::as_str).collect();
                bail!("Unknown preset {name}. Presets: {}.", names.join(", "));
            }
        },
        None => None,
    };
    let context = opts.context.iter().map(|p| context::load(p)).collect::<anyhow::Result<Vec<_>>>()?;
    if !opts.yes && !s.tty {
        bail!(NEED_YES);
    }
    let models = s.client.list_models().await.map_err(|e| anyhow::anyhow!("Couldn't load models: {}", e.message))?;
    if models.is_empty() {
        bail!("No models available.");
    }
    let slugs: Vec<String> = models.into_iter().map(|m| m.slug).collect();
    let p = preset.as_ref().map(|(_, p)| p.clone()).unwrap_or_default();
    let stage = |m: &config::StageModel| config::resolve_model(&p.stage(m), &slugs);
    let mut rc = s.cfg.run_config(
        stage(&s.cfg.planner),
        stage(&s.cfg.researcher),
        config::resolve_model(&s.cfg.compactor, &slugs),
        stage(&s.cfg.synthesizer),
        opts.agents.or(p.agents).unwrap_or(s.cfg.default_agents),
    );
    p.apply(&mut rc);
    if let Some((name, _)) = preset {
        rc.preset = name;
    }
    rc.verify_citations |= opts.verify;
    let sigint = interrupt()?;
    let started = Instant::now();
    let run = Run::start(s.ctx, opts.target.trim().to_string(), rc, context, s.tty && !opts.no_clarify);
    Ok(drive(run, s.rx, sigint, &opts, s.tty, started).await)
}

async fn resume(opts: Opts) -> anyhow::Result<i32> {
    let id = &opts.target;
    let rec = store::load(id)?;
    if rec.status == RunStatus::Done {
        bail!("Research {id} is already done. Use rsc show {id}.");
    }
    let s = setup()?;
    let needs_plan = matches!(rec.status, RunStatus::Review | RunStatus::GapReview | RunStatus::Clarifying)
        || rec.perspectives.is_empty();
    if needs_plan && !opts.yes && !s.tty {
        bail!(NEED_YES);
    }
    let sigint = interrupt()?;
    let started = Instant::now();
    let run = Run::resume(s.ctx, rec, vec![]);
    Ok(drive(run, s.rx, sigint, &opts, s.tty, started).await)
}

enum Line {
    Text(String),
    Eof,
    Interrupted,
}

struct Prompter {
    lines: Option<Lines<BufReader<Stdin>>>,
}

impl Prompter {
    async fn read(&mut self, sigint: &mut Interrupt) -> Line {
        let lines = self.lines.get_or_insert_with(|| BufReader::new(tokio::io::stdin()).lines());
        tokio::select! {
            _ = sigint.recv() => Line::Interrupted,
            l = lines.next_line() => match l {
                Ok(Some(t)) => Line::Text(t),
                _ => Line::Eof,
            },
        }
    }
}

/// Prints status and agent transitions to stderr, only when something changed.
#[derive(Default)]
struct Progress {
    status: Option<RunStatus>,
    agents: Vec<AgentStatus>,
}

impl Progress {
    fn print(&mut self, run: &Run) {
        let p = term::err();
        let (status, agents) = run.with(|s| {
            let a: Vec<_> = s
                .record
                .agents
                .iter()
                .map(|a| (a.title.clone(), a.status, a.error.clone().unwrap_or_default()))
                .collect();
            (s.record.status, a)
        });
        // A late status (synthesizing and on) is printed after the agent lines that led to it.
        let late = matches!(
            status,
            RunStatus::GapReview
                | RunStatus::Synthesizing
                | RunStatus::Done
                | RunStatus::Failed
                | RunStatus::Cancelled
                | RunStatus::Interrupted
        );
        let changed = self.status != Some(status);
        self.status = Some(status);
        if changed && !late {
            term::note(paint_status(p, status, status.label()));
        }
        let n = agents.len();
        for (i, (title, st, err)) in agents.iter().enumerate() {
            let prev = self.agents.get(i).copied().unwrap_or(AgentStatus::Pending);
            if prev == *st {
                continue;
            }
            match st {
                AgentStatus::Running => {
                    term::note(format!("{} {}: {title}", p.dim(format!("[{}/{n}]", i + 1)), p.cyan("started")))
                }
                AgentStatus::Done => {
                    term::note(format!("{} {}: {title}", p.dim(format!("[{}/{n}]", i + 1)), p.green("done")))
                }
                AgentStatus::Failed => {
                    term::note(format!("{} {}: {title}: {err}", p.dim(format!("[{}/{n}]", i + 1)), p.red("failed")))
                }
                AgentStatus::Pending => {}
            }
        }
        if changed && late {
            term::note(paint_status(p, status, status.label()));
        }
        self.agents = agents.into_iter().map(|a| a.1).collect();
    }
}

struct Driver<'a> {
    run: Arc<Run>,
    sigint: Interrupt,
    prompter: Prompter,
    opts: &'a Opts,
    tty: bool,
}

impl Driver<'_> {
    fn print_angles(ps: &[store::Perspective]) {
        let p = term::err();
        for (i, a) in ps.iter().enumerate() {
            eprintln!("  {} {} — {}", p.dim(format!("{}.", i + 1)), p.bold(&a.title), a.brief);
        }
        for (i, j) in plan::overlaps(ps) {
            eprintln!("  {} angles {} and {} look similar", p.yellow(p.bold("warning:")), i + 1, j + 1);
        }
    }

    /// Accept the plan (`--yes` or the user's answer) or cancel the run.
    async fn review(&mut self) {
        let ps = self.run.with(|s| s.record.perspectives.clone());
        let p = term::err();
        eprintln!("{}", p.bold(format!("Plan ({} angles):", ps.len())));
        let docs = self.run.with(|s| s.record.context.clone());
        if !docs.is_empty() {
            let names: Vec<_> = docs
                .iter()
                .map(|d| if d.truncated { format!("{} (truncated)", d.name) } else { d.name.clone() })
                .collect();
            eprintln!("{} {}", p.dim("Attached files (sent to the model):"), names.join(", "));
        }
        Self::print_angles(&ps);
        if self.opts.yes {
            self.run.launch();
            return;
        }
        eprint!("{} {} ", p.bold("Start research?"), p.dim("[Y/n]"));
        match self.prompter.read(&mut self.sigint).await {
            Line::Text(t) if !matches!(t.trim().to_lowercase().as_str(), "n" | "no") => self.run.launch(),
            _ => self.run.cancel(),
        }
    }

    /// Approve or skip the follow-up round the reviewer proposed.
    async fn gaps(&mut self) {
        let (ps, notes) = self.run.with(|s| (s.record.gaps.clone(), s.record.gap_notes.clone()));
        let p = term::err();
        eprintln!("{}", p.bold(format!("Follow-up round proposed ({} angles):", ps.len())));
        for l in notes.lines() {
            eprintln!("  {}", p.dim(l));
        }
        Self::print_angles(&ps);
        if self.opts.yes {
            self.run.approve_gaps();
            return;
        }
        eprint!("{} {} ", p.bold("Run follow-up round?"), p.dim("[Y/n]"));
        match self.prompter.read(&mut self.sigint).await {
            Line::Text(t) if matches!(t.trim().to_lowercase().as_str(), "n" | "no") => self.run.skip_gaps(),
            Line::Text(_) => self.run.approve_gaps(),
            Line::Eof => self.run.skip_gaps(),
            Line::Interrupted => self.run.cancel(),
        }
    }

    async fn clarify(&mut self) {
        if !(self.tty && !self.opts.no_clarify) {
            self.run.answer(vec![]);
            return;
        }
        let qs = self.run.with(|s| s.record.questions.clone());
        let p = term::err();
        eprintln!("{} {}", p.bold("The planner has questions"), p.dim("(Enter skips):"));
        let mut answers = Vec::new();
        for (i, q) in qs.iter().enumerate() {
            eprintln!("{} {}", p.dim(format!("{}.", i + 1)), p.bold(q));
            eprint!("{} ", p.cyan(">"));
            match self.prompter.read(&mut self.sigint).await {
                Line::Text(t) => answers.push(t),
                Line::Eof => answers.push(String::new()),
                Line::Interrupted => {
                    self.run.cancel();
                    return;
                }
            }
        }
        self.run.answer(answers);
    }
}

async fn drive(
    run: Arc<Run>,
    mut rx: UnboundedReceiver<RunEvent>,
    sigint: Interrupt,
    opts: &Opts,
    tty: bool,
    started: Instant,
) -> i32 {
    term::note(format!("research {}", term::err().cyan(run.with(|s| s.record.id.clone()))));
    let mut d = Driver { run: run.clone(), sigint, prompter: Prompter { lines: None }, opts, tty };
    let mut progress = Progress::default();
    // A resumed run may already be waiting on the user.
    match run.status() {
        RunStatus::Review => d.review().await,
        RunStatus::Clarifying => d.clarify().await,
        RunStatus::GapReview => d.gaps().await,
        _ => {}
    }
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        if run.cancel.is_cancelled() {
            break;
        }
        tokio::select! {
            _ = d.sigint.recv() => {
                run.cancel();
                break;
            }
            ev = rx.recv() => match ev {
                Some(RunEvent::PlanReady) => d.review().await,
                Some(RunEvent::GapsReady) => d.gaps().await,
                Some(RunEvent::Questions) => d.clarify().await,
                Some(RunEvent::Done | RunEvent::Failed(_)) | None => break,
                Some(RunEvent::Toast(t)) => term::note(t),
                Some(RunEvent::ChatFailed { .. }) => {}
            },
            _ = tick.tick() => progress.print(&run),
        }
    }
    progress.print(&run);
    finish(&run, opts, started)
}

/// Print the outcome and return the exit code.
fn finish(run: &Run, opts: &Opts, started: Instant) -> i32 {
    let (rec, report) = run.with(|s| (s.record.clone(), s.final_report.clone()));
    let mut code = match rec.status {
        RunStatus::Done if rec.budget_hit => 2,
        RunStatus::Done => 0,
        RunStatus::Cancelled => 130,
        _ => 1,
    };
    if rec.status == RunStatus::Failed
        && let Some(e) = &rec.error
    {
        let rid = e.request_id.as_ref().map(|r| format!(" (request {r})")).unwrap_or_default();
        term::error(format!("research failed: {}{rid}", e.message));
    }
    if let Some(report) = &report {
        if let Some(path) = &opts.output {
            match paths::atomic_write(path, report.as_bytes()) {
                Ok(()) => term::note(format!("report saved to {}", term::err().bold(path.display()))),
                Err(e) => {
                    term::error(format!("Couldn't write {}: {e}", path.display()));
                    code = 1;
                }
            }
        } else if !opts.json {
            print_report(report);
        }
    }
    if opts.json {
        let agents: Vec<Value> = rec
            .agents
            .iter()
            .map(|a| {
                json!({
                    "title": a.title,
                    "status": a.status,
                    "error": a.error,
                    "turns": a.turns,
                    "tokens": a.usage.tokens(),
                    "cost_usd": a.cost_usd,
                })
            })
            .collect();
        let cited = || rec.sources.iter().filter(|s| s.in_final);
        let links = |st: LinkStatus| cited().filter(|s| s.status == st).count();
        let verdicts = |v: Support| rec.sources.iter().flat_map(|s| &s.verdicts).filter(|d| d.verdict == v).count();
        let out = json!({
            "id": rec.id,
            "question": rec.question,
            "status": rec.status.label(),
            "exit_code": code,
            "elapsed_secs": started.elapsed().as_secs_f64(),
            "tokens": rec.total_usage().tokens(),
            "cost_usd": rec.cost_usd_total,
            "cost_partial": rec.cost_partial,
            "budget_hit": rec.budget_hit,
            "report_path": report.as_ref().map(|_| rec.dir().join("report.md").display().to_string()),
            "output": opts.output.as_ref().map(|p| p.display().to_string()),
            "report_chars": report.as_ref().map(|r| r.chars().count()),
            "report_words": report.as_ref().map(|r| r.split_whitespace().count()),
            "agents_failed": rec.agents.iter().filter(|a| a.status == AgentStatus::Failed).count(),
            "gap_agents": rec.agents.iter().filter(|a| a.followup).count(),
            "config": rec.config,
            "citations": {
                "cited_sources": cited().count(),
                "invalid_numbers": rec.synthesis.invalid_citations.len(),
                "links": {
                    "live": links(LinkStatus::Live),
                    "dead": links(LinkStatus::Dead),
                    "unknown": links(LinkStatus::Unknown),
                    "unchecked": links(LinkStatus::Unchecked),
                },
                "model_supplied": cited().filter(|s| s.model_supplied).count(),
                "verdicts": {
                    "supported": verdicts(Support::Supported),
                    "partial": verdicts(Support::Partial),
                    "unsupported": verdicts(Support::Unsupported),
                },
            },
            "sources": {"total": rec.sources.len(), "cited": rec.sources.iter().filter(|s| s.in_final).count()},
            "agents": agents,
            "error": rec.error,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    }
    let p = term::err();
    term::note(format!(
        "{} · {} · {}",
        paint_status(p, rec.status, rec.status.label()),
        p.dim(fmt::badge(&rec)),
        p.cyan(&rec.id)
    ));
    let _ = std::io::stdout().flush();
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn run_with_all_flags() {
        let o = parse_opts(
            "run",
            &args(&["What is X?", "--agents", "3", "--verify", "-y", "--no-clarify", "-o", "out.md", "--json"]),
        )
        .unwrap();
        assert_eq!(
            o,
            Opts {
                target: "What is X?".into(),
                agents: Some(3),
                verify: true,
                yes: true,
                no_clarify: true,
                output: Some("out.md".into()),
                json: true,
                ..Opts::default()
            }
        );
    }

    #[test]
    fn export_flags() {
        let o = parse_opts("export", &args(&["id1", "--format", "html", "-o", "x.html"])).unwrap();
        assert_eq!(
            o,
            Opts { target: "id1".into(), format: Some(Format::Html), output: Some("x.html".into()), ..Opts::default() }
        );
        let e = parse_opts("export", &args(&["id1", "--format", "docx"])).unwrap_err();
        assert_eq!(e.to_string(), "--format must be md, html, json or pdf.");
        let e = parse_opts("run", &args(&["q", "--format", "md"])).unwrap_err();
        assert_eq!(e.to_string(), "Unknown option --format.");
    }

    #[test]
    fn preset_and_context_flags() {
        let o =
            parse_opts("run", &args(&["q", "--preset", "deep", "--context", "a.md", "--context", "b.pdf"])).unwrap();
        assert_eq!(o.preset.as_deref(), Some("deep"));
        assert_eq!(o.context, vec![PathBuf::from("a.md"), PathBuf::from("b.pdf")]);
        assert_eq!(parse_opts("run", &args(&["q", "--preset"])).unwrap_err().to_string(), "--preset needs a name.");
        assert_eq!(parse_opts("run", &args(&["q", "--context"])).unwrap_err().to_string(), "--context needs a file.");
        assert_eq!(
            parse_opts("resume", &args(&["id", "--preset", "x"])).unwrap_err().to_string(),
            "--preset only applies to rsc run."
        );
        assert_eq!(
            parse_opts("resume", &args(&["id", "--context", "x"])).unwrap_err().to_string(),
            "--context only applies to rsc run."
        );
        let many = ["q", "--context", "1", "--context", "2", "--context", "3", "--context", "4", "--context", "5"];
        assert_eq!(parse_opts("run", &args(&many)).unwrap_err().to_string(), "At most 4 context files.");
    }

    #[test]
    fn agents_out_of_range_rejected() {
        for bad in ["0", "100", "x"] {
            let e = parse_opts("run", &args(&["q", "--agents", bad])).unwrap_err();
            assert_eq!(e.to_string(), "--agents must be 1–99 or auto.");
        }
        assert_eq!(parse_opts("run", &args(&["q", "--agents", "auto"])).unwrap().agents, Some(0));
        assert!(parse_opts("run", &args(&["q", "--agents"])).is_err());
    }

    #[test]
    fn agents_rejected_on_resume() {
        let e = parse_opts("resume", &args(&["id", "--agents", "2"])).unwrap_err();
        assert_eq!(e.to_string(), "--agents only applies to rsc run.");
    }

    #[test]
    fn verify_rejected_on_resume() {
        let e = parse_opts("resume", &args(&["id", "--verify"])).unwrap_err();
        assert_eq!(e.to_string(), "--verify only applies to rsc run.");
    }

    #[test]
    fn unknown_flag_rejected() {
        let e = parse_opts("run", &args(&["q", "--bogus"])).unwrap_err();
        assert_eq!(e.to_string(), "Unknown option --bogus.");
        assert!(parse_opts("show", &args(&["id", "--json"])).is_err());
    }

    #[test]
    fn positional_count_enforced() {
        for (cmd, a) in
            [("run", vec![]), ("run", vec!["a", "b"]), ("resume", vec![]), ("show", vec!["a", "b"]), ("ls", vec!["a"])]
        {
            let e = parse_opts(cmd, &args(&a)).unwrap_err();
            assert!(e.is::<UsageError>(), "{cmd} {a:?}");
        }
        assert!(parse_opts("ls", &[]).is_ok());
    }

    #[test]
    fn blank_question_rejected() {
        let e = parse_opts("run", &args(&["  "])).unwrap_err();
        assert_eq!(e.to_string(), "Question is empty.");
    }
}
