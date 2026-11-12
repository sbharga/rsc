//! Headless commands: `rsc run`, `rsc resume`, `rsc ls`, `rsc show`. No ratatui.
//!
//! Never calls `store::mark_interrupted`: that would flip a run that a TUI in another process is still working on.

use crate::api::Client;
use crate::auth::{self, Account, AuthHandle};
use crate::config::{self, Config, MAX_AGENTS};
use crate::paths;
use crate::pricing::Pricing;
use crate::research::store::{self, AgentStatus, RunStatus};
use crate::research::{Ctx, Run, RunEvent};
use crate::ui::fmt;
use anyhow::{Context, bail};
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader, Lines, Stdin};
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

pub const USAGE: &str = "usage: rsc [--version | update]
       rsc run \"<question>\" [--agents N] [--yes] [--no-clarify] [-o FILE] [--json]
       rsc resume <id> [--yes] [--no-clarify] [-o FILE] [--json]
       rsc ls
       rsc show <id>";

const NEED_YES: &str = "Pass --yes to accept the plan when not running in a terminal.";

#[derive(Debug, Default, PartialEq)]
struct Opts {
    /// Question for `run`, id for `resume` and `show`; empty for `ls`.
    target: String,
    agents: Option<u8>,
    yes: bool,
    no_clarify: bool,
    output: Option<PathBuf>,
    json: bool,
}

fn parse_opts(cmd: &str, args: &[String]) -> anyhow::Result<Opts> {
    let runs = matches!(cmd, "run" | "resume");
    let mut o = Opts::default();
    let mut positional = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--agents" if cmd == "run" => {
                let v = it.next().and_then(|v| v.parse::<u8>().ok()).filter(|n| (1..=MAX_AGENTS).contains(n));
                o.agents = Some(v.with_context(|| format!("--agents must be 1–{MAX_AGENTS}."))?);
            }
            "--agents" if cmd == "resume" => bail!("--agents only applies to rsc run."),
            "--yes" | "-y" if runs => o.yes = true,
            "--no-clarify" if runs => o.no_clarify = true,
            "--json" if runs => o.json = true,
            "-o" | "--output" if runs => {
                o.output = Some(PathBuf::from(it.next().with_context(|| format!("{a} needs a file."))?));
            }
            x if x.starts_with('-') => bail!("Unknown option {x}."),
            _ => positional.push(a.clone()),
        }
    }
    match (cmd, positional.len()) {
        ("ls", 0) => {}
        ("run" | "resume" | "show", 1) => o.target = positional.remove(0),
        _ => bail!("{USAGE}"),
    }
    if cmd == "run" && o.target.trim().is_empty() {
        bail!("Question is empty.");
    }
    Ok(o)
}

pub async fn main(args: &[String]) -> anyhow::Result<i32> {
    let cmd = args.first().map(String::as_str).unwrap_or_default();
    let opts = parse_opts(cmd, &args[1..])?;
    match cmd {
        "ls" => ls(),
        "show" => show(&opts.target),
        "run" => run(opts).await,
        _ => resume(opts).await,
    }
}

fn ls() -> anyhow::Result<i32> {
    for r in store::list() {
        println!(
            "{}  {:<12}  {}  {}",
            r.id,
            r.status.label(),
            fmt::badge(&r),
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
        eprintln!("rsc: {b}");
    }
    let pricing = Pricing::load();
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
    if !opts.yes && !s.tty {
        bail!(NEED_YES);
    }
    let models = s.client.list_models().await.map_err(|e| anyhow::anyhow!("Couldn't load models: {}", e.message))?;
    if models.is_empty() {
        bail!("No models available.");
    }
    let slugs: Vec<String> = models.into_iter().map(|m| m.slug).collect();
    let stage = |m: &config::StageModel| config::resolve_model(m, &slugs);
    let rc = s.cfg.run_config(
        stage(&s.cfg.planner),
        stage(&s.cfg.researcher),
        stage(&s.cfg.compactor),
        stage(&s.cfg.synthesizer),
        opts.agents.unwrap_or(s.cfg.default_agents),
    );
    let sigint = signal(SignalKind::interrupt())?;
    let started = Instant::now();
    let run = Run::start(s.ctx, opts.target.trim().to_string(), rc, s.tty && !opts.no_clarify);
    Ok(drive(run, s.rx, sigint, &opts, s.tty, started).await)
}

async fn resume(opts: Opts) -> anyhow::Result<i32> {
    let id = &opts.target;
    let rec = store::load(id)?;
    if rec.status == RunStatus::Done {
        bail!("Research {id} is already done. Use rsc show {id}.");
    }
    let s = setup()?;
    let needs_plan = matches!(rec.status, RunStatus::Review | RunStatus::Clarifying) || rec.perspectives.is_empty();
    if needs_plan && !opts.yes && !s.tty {
        bail!(NEED_YES);
    }
    let sigint = signal(SignalKind::interrupt())?;
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
    async fn read(&mut self, sigint: &mut Signal) -> Line {
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
            RunStatus::Synthesizing
                | RunStatus::Done
                | RunStatus::Failed
                | RunStatus::Cancelled
                | RunStatus::Interrupted
        );
        let changed = self.status != Some(status);
        self.status = Some(status);
        if changed && !late {
            eprintln!("rsc: {}", status.label());
        }
        let n = agents.len();
        for (i, (title, st, err)) in agents.iter().enumerate() {
            let prev = self.agents.get(i).copied().unwrap_or(AgentStatus::Pending);
            if prev == *st {
                continue;
            }
            match st {
                AgentStatus::Running => eprintln!("rsc: [{}/{n}] started: {title}", i + 1),
                AgentStatus::Done => eprintln!("rsc: [{}/{n}] done: {title}", i + 1),
                AgentStatus::Failed => eprintln!("rsc: [{}/{n}] failed: {title}: {err}", i + 1),
                AgentStatus::Pending => {}
            }
        }
        if changed && late {
            eprintln!("rsc: {}", status.label());
        }
        self.agents = agents.into_iter().map(|a| a.1).collect();
    }
}

struct Driver<'a> {
    run: Arc<Run>,
    sigint: Signal,
    prompter: Prompter,
    opts: &'a Opts,
    tty: bool,
}

impl Driver<'_> {
    /// Accept the plan (`--yes` or the user's answer) or cancel the run.
    async fn review(&mut self) {
        let ps = self.run.with(|s| s.record.perspectives.clone());
        eprintln!("Plan ({} angles):", ps.len());
        for (i, p) in ps.iter().enumerate() {
            eprintln!("  {}. {} — {}", i + 1, p.title, p.brief);
        }
        if self.opts.yes {
            self.run.launch();
            return;
        }
        eprint!("Start research? [Y/n] ");
        match self.prompter.read(&mut self.sigint).await {
            Line::Text(t) if !matches!(t.trim().to_lowercase().as_str(), "n" | "no") => self.run.launch(),
            _ => self.run.cancel(),
        }
    }

    async fn clarify(&mut self) {
        if !(self.tty && !self.opts.no_clarify) {
            self.run.answer(vec![]);
            return;
        }
        let qs = self.run.with(|s| s.record.questions.clone());
        eprintln!("The planner has questions (Enter skips):");
        let mut answers = Vec::new();
        for (i, q) in qs.iter().enumerate() {
            eprintln!("{}. {q}", i + 1);
            eprint!("> ");
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
    sigint: Signal,
    opts: &Opts,
    tty: bool,
    started: Instant,
) -> i32 {
    eprintln!("rsc: research {}", run.with(|s| s.record.id.clone()));
    let mut d = Driver { run: run.clone(), sigint, prompter: Prompter { lines: None }, opts, tty };
    let mut progress = Progress::default();
    // A resumed run may already be waiting on the user.
    match run.status() {
        RunStatus::Review => d.review().await,
        RunStatus::Clarifying => d.clarify().await,
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
                Some(RunEvent::Questions) => d.clarify().await,
                Some(RunEvent::Done | RunEvent::Failed(_)) | None => break,
                Some(RunEvent::Toast(t)) => eprintln!("rsc: {t}"),
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
        eprintln!("rsc: research failed: {}{rid}", e.message);
    }
    if let Some(report) = &report {
        if let Some(path) = &opts.output {
            match paths::atomic_write(path, report.as_bytes()) {
                Ok(()) => eprintln!("rsc: report saved to {}", path.display()),
                Err(e) => {
                    eprintln!("rsc: Couldn't write {}: {e}", path.display());
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
            "sources": {"total": rec.sources.len(), "cited": rec.sources.iter().filter(|s| s.in_final).count()},
            "agents": agents,
            "error": rec.error,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    }
    eprintln!("rsc: {} · {} · {}", rec.status.label(), fmt::badge(&rec), rec.id);
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
        let o =
            parse_opts("run", &args(&["What is X?", "--agents", "3", "-y", "--no-clarify", "-o", "out.md", "--json"]))
                .unwrap();
        assert_eq!(
            o,
            Opts {
                target: "What is X?".into(),
                agents: Some(3),
                yes: true,
                no_clarify: true,
                output: Some("out.md".into()),
                json: true,
            }
        );
    }

    #[test]
    fn agents_out_of_range_rejected() {
        for bad in ["0", "100", "x"] {
            let e = parse_opts("run", &args(&["q", "--agents", bad])).unwrap_err();
            assert_eq!(e.to_string(), "--agents must be 1–99.");
        }
        assert!(parse_opts("run", &args(&["q", "--agents"])).is_err());
    }

    #[test]
    fn agents_rejected_on_resume() {
        let e = parse_opts("resume", &args(&["id", "--agents", "2"])).unwrap_err();
        assert_eq!(e.to_string(), "--agents only applies to rsc run.");
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
            assert_eq!(e.to_string(), USAGE, "{cmd} {a:?}");
        }
        assert!(parse_opts("ls", &[]).is_ok());
    }

    #[test]
    fn blank_question_rejected() {
        let e = parse_opts("run", &args(&["  "])).unwrap_err();
        assert_eq!(e.to_string(), "Question is empty.");
    }
}
