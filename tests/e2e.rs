//! Full runs of the real `rsc` binary against a scripted Responses API mock. No network, scratch `HOME`.

mod support;

use serde_json::{Value, json};
use std::process::Output;
use support::*;

const BASE: &str = "default_agents = 2\nmax_parallel = 2\n";

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

async fn run(env: &Env, args: &[&str]) -> Output {
    finish(env.cmd(args).spawn().unwrap()).await
}

/// Planner → `titles`, researchers write a report, synthesizer answers.
fn normal(req: &Req, titles: &[&str]) -> Reply {
    match &req.kind {
        Kind::Planner => Reply::Output(vec![search("q"), plan(titles)]),
        Kind::Researcher(t) => Reply::Output(vec![msg(&format!("Report {t}: [src](https://example.com/{t})"))]),
        Kind::Synth => Reply::Output(vec![msg("Final answer [1].")]),
        Kind::Compactor => Reply::Output(vec![msg("summary")]),
        Kind::GroupSynth => Reply::Output(vec![msg("Group summary [1].")]),
        Kind::Gaps => Reply::Output(vec![gaps(&[])]),
        Kind::Verify => Reply::Output(vec![msg(r#"{"verdicts":[]}"#)]),
    }
}

fn blocked_fetch() -> Value {
    fetch_call("http://127.0.0.1:9/x", "c1")
}

fn researcher_input_has_output(body: &Value, expected: &str) -> bool {
    body["input"].as_array().unwrap().iter().any(|i| i["type"] == "function_call_output" && i["output"] == expected)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_path_writes_report() {
    let cfg = format!("{BASE}fetch_url_enabled = true\n");
    let env = Env::new(&cfg, |r| match (&r.kind, r.n) {
        (Kind::Researcher(_), 0) => Reply::Output(vec![blocked_fetch()]),
        _ => normal(r, &["A", "B"]),
    })
    .await;
    let o = run(&env, &["run", "What is X?", "--yes"]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let stdout = out(&o);
    assert!(stdout.contains("Final answer [1]."), "{stdout}");
    assert!(stdout.contains("## Sources"), "{stdout}");

    assert_eq!(env.mock.count(&Kind::GroupSynth), 0);
    let (id, rec) = env.record();
    assert_eq!(rec["status"], "done", "{}", err(&o));
    assert!(rec["agents"].as_array().unwrap().iter().all(|a| a["status"] == "done"));
    let dir = env.home.join(".rsc/research").join(&id);
    assert!(dir.join("agents/1.md").exists() && dir.join("agents/2.md").exists());

    let planner = &env.mock.bodies(&Kind::Planner)[0];
    assert!(planner["instructions"].as_str().unwrap().contains("Do not ask any more questions"));
    for t in ["A", "B"] {
        let bodies = env.mock.bodies(&Kind::Researcher(t.into()));
        assert!(researcher_input_has_output(&bodies[1], "error: blocked address"), "{t}: {:?}", bodies[1]["input"]);
    }

    let ls = run(&env, &["ls"]).await;
    let ls = out(&ls);
    assert!(ls.contains(&id) && ls.contains("done"), "{ls}");
    let show = run(&env, &["show", &id]).await;
    let report = std::fs::read_to_string(dir.join("report.md")).unwrap();
    assert_eq!(out(&show).trim_end(), report.trim_end());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_researcher_still_synthesizes() {
    let env = Env::new(BASE, |r| match &r.kind {
        Kind::Researcher(t) if t == "B" => {
            Reply::Error(400, json!({"error":{"message":"boom","code":"invalid_request"}}))
        }
        _ => normal(r, &["A", "B"]),
    })
    .await;
    let path = env.home.join("out.md");
    let o = run(&env, &["run", "q", "--yes", "--json", "-o", path.to_str().unwrap()]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: Value = serde_json::from_str(&out(&o)).unwrap_or_else(|e| panic!("{e}: {}", out(&o)));
    assert_eq!(j["status"], "done", "{}", err(&o));
    assert_eq!(j["exit_code"], 0);
    assert_eq!(j["agents"][1]["status"], "failed");
    assert_eq!(j["agents"][1]["error"], "boom");
    assert_eq!(j["agents_failed"], 1);
    assert_eq!(j["citations"]["invalid_numbers"], 0);
    assert_eq!(j["config"]["agents"], 2);
    assert!(j["report_words"].is_number(), "{j}");
    assert_eq!(j["output"], path.to_str().unwrap());
    assert!(std::fs::read_to_string(&path).unwrap().contains("Final"));
    let synth = &env.mock.bodies(&Kind::Synth)[0];
    assert!(synth["input"].to_string().contains("(this researcher failed: boom)"), "{}", synth["input"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_limit_fails_run() {
    let code = "subscription_sharing_usage_limit_exceeded";
    let env = Env::new(BASE, move |r| match &r.kind {
        Kind::Researcher(_) => Reply::Error(429, json!({"error":{"code":code,"message":"limit"}})),
        _ => normal(r, &["A", "B"]),
    })
    .await;
    let o = run(&env, &["run", "q", "--yes"]).await;
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let (_, rec) = env.record();
    assert_eq!(rec["status"], "failed");
    assert_eq!(rec["error"]["code"], code);
    assert_eq!(env.mock.count(&Kind::Synth), 0);
    assert!(err(&o).contains("limit"), "{}", err(&o));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_mid_run_exits_130() {
    let env = Env::new(BASE, |r| match &r.kind {
        Kind::Researcher(_) => Reply::Hang,
        _ => normal(r, &["A", "B"]),
    })
    .await;
    let child = env.cmd(&["run", "q", "--yes"]).spawn().unwrap();
    let pid = child.id().unwrap();
    let (a, b) = (Kind::Researcher("A".into()), Kind::Researcher("B".into()));
    until(|| env.mock.count(&a) + env.mock.count(&b) == 2).await;
    sigint(pid);
    let o = finish(child).await;
    assert_eq!(o.status.code(), Some(130), "{}", err(&o));
    let (_, rec) = env.record();
    assert_eq!(rec["status"], "cancelled");
    assert!(rec["agents"].as_array().unwrap().iter().all(|a| a["status"] == "pending"), "{rec}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_skips_finished_agents() {
    let env = Env::new(BASE, |r| match (&r.kind, r.n) {
        (Kind::Researcher(t), 0) if t == "B" => Reply::Hang,
        _ => normal(r, &["A", "B"]),
    })
    .await;
    let (a, b) = (Kind::Researcher("A".into()), Kind::Researcher("B".into()));
    let mut child = env.cmd(&["run", "q", "--yes"]).spawn().unwrap();
    until(|| env.mock.count(&b) == 1 && env.try_record().is_some_and(|(_, r)| r["agents"][0]["status"] == "done"))
        .await;
    child.kill().await.unwrap();
    let (id, rec) = env.record();
    assert_eq!(rec["status"], "researching");

    let o = run(&env, &["resume", &id]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(env.mock.count(&a), 1);
    assert_eq!(env.mock.count(&b), 2);
    assert_eq!(env.record().1["status"], "done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capability_degradation_retries_without_field() {
    let unsupported = |param: &str| {
        Reply::Error(
            400,
            json!({"error":{"code":"subscription_sharing_unsupported_capability","param":param,"message":"no"}}),
        )
    };
    let env = Env::new("fetch_url_enabled = true\n", move |r| {
        let has_namespace = r.body["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["type"] == "namespace"));
        if !r.body["reasoning"]["summary"].is_null() {
            unsupported("reasoning.summary")
        } else if !r.body["context_management"].is_null() {
            unsupported("context_management")
        } else if has_namespace {
            Reply::Error(400, json!({"error":{"message":"bad tool","param":"tools[1]"}}))
        } else {
            normal(r, &["A"])
        }
    })
    .await;
    let o = run(&env, &["run", "q", "--yes", "--agents", "1"]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("fetch_url unsupported on this route"), "{}", err(&o));
    let planner = env.mock.bodies(&Kind::Planner);
    assert_eq!(planner.len(), 2);
    assert!(!planner[0]["reasoning"]["summary"].is_null());
    assert!(planner[1]["reasoning"]["summary"].is_null());
    let a = env.mock.bodies(&Kind::Researcher("A".into()));
    assert_eq!(a.len(), 3);
    let last = a.last().unwrap();
    assert!(last["context_management"].is_null());
    assert!(!last["tools"].as_array().unwrap().iter().any(|t| t["type"] == "namespace"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_5xx() {
    let env = Env::new(BASE, |r| match (&r.kind, r.n) {
        (Kind::Researcher(_), 0) => Reply::Error(503, json!({"error":{"message":"busy"}})),
        _ => normal(r, &["A"]),
    })
    .await;
    let o = run(&env, &["run", "q", "--yes", "--agents", "1"]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(env.mock.count(&Kind::Researcher("A".into())), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budget_cap_exits_2() {
    let cfg = "default_agents = 2\nmax_parallel = 1\nmax_run_tokens = 200\n";
    let env = Env::new(cfg, |r| normal(r, &["A", "B"])).await;
    let o = run(&env, &["run", "q", "--yes"]).await;
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(out(&o).contains("run budget was reached"), "{}", out(&o));
    let (_, rec) = env.record();
    assert_eq!(rec["budget_hit"], true);
    let skipped = rec["agents"].as_array().unwrap().iter().filter(|a| a["error"] == "skipped: run budget reached");
    assert_eq!(skipped.count(), 1, "{rec}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_without_yes_requires_tty() {
    let env = Env::new(BASE, |r| normal(r, &["A", "B"])).await;
    let o = run(&env, &["run", "q"]).await;
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("--yes"), "{}", err(&o));
    assert_eq!(env.mock.hits(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_continues_from_checkpoint() {
    let cfg = format!("{BASE}fetch_url_enabled = true\n");
    let env = Env::new(&cfg, |r| match (&r.kind, r.n) {
        (Kind::Researcher(t), n) if t == "B" => match n {
            0 => Reply::Output(vec![blocked_fetch()]),
            1 => Reply::Hang,
            _ => normal(r, &["A", "B"]),
        },
        _ => normal(r, &["A", "B"]),
    })
    .await;
    let b = Kind::Researcher("B".into());
    let mut child = env.cmd(&["run", "q", "--yes"]).spawn().unwrap();
    let state = env.home.join(".rsc/research");
    let find_state =
        || std::fs::read_dir(&state).ok()?.flatten().map(|e| e.path().join("agents/2.state.json")).find(|p| p.exists());
    until(|| find_state().is_some() && env.mock.count(&b) == 2).await;
    child.kill().await.unwrap();
    let state_path = find_state().unwrap();
    let st: Value = serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(st["turns"], 1);
    assert_eq!(st["input"].as_array().unwrap().len(), 3);

    let (id, _) = env.record();
    let o = run(&env, &["resume", &id]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let bodies = env.mock.bodies(&b);
    assert_eq!(bodies.len(), 3);
    assert!(researcher_input_has_output(&bodies[2], "error: blocked address"), "{}", bodies[2]["input"]);
    assert_eq!(env.record().1["agents"][1]["turns"], 2);
    assert!(!state_path.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_restarts_after_retryable_failure() {
    let env = Env::new("", |r| match (&r.kind, r.n) {
        (Kind::Researcher(_), 0..=3) => Reply::Error(503, json!({"error":{"message":"busy"}})),
        _ => normal(r, &["A"]),
    })
    .await;
    let o = run(&env, &["run", "q", "--yes", "--agents", "1"]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(env.mock.count(&Kind::Researcher("A".into())), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_is_one_shot_per_agent() {
    let env = Env::new("", |r| match &r.kind {
        Kind::Researcher(_) => Reply::Error(503, json!({"error":{"message":"busy"}})),
        _ => normal(r, &["A"]),
    })
    .await;
    let o = run(&env, &["run", "q", "--yes", "--agents", "1"]).await;
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert_eq!(env.mock.count(&Kind::Researcher("A".into())), 8);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restarts_are_capped_per_run() {
    let env = Env::new("max_parallel = 4\n", |r| match &r.kind {
        Kind::Researcher(_) => Reply::Error(503, json!({"error":{"message":"busy"}})),
        _ => normal(r, &["A", "B", "C", "D"]),
    })
    .await;
    let o = run(&env, &["run", "q", "--yes", "--agents", "4"]).await;
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let total: usize = ["A", "B", "C", "D"].iter().map(|t| env.mock.count(&Kind::Researcher(t.to_string()))).sum();
    assert_eq!(total, 28);
}

const GAP_BASE: &str = "default_agents = 2\nmax_parallel = 2\ngap_round = true\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gap_round_runs_followup_agents() {
    let env = Env::new(GAP_BASE, |r| match &r.kind {
        Kind::Gaps => Reply::Output(vec![gaps(&["C"])]),
        _ => normal(r, &["A", "B"]),
    })
    .await;
    let o = run(&env, &["run", "q", "--yes", "--json"]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(env.mock.count(&Kind::Gaps), 1);
    assert!(env.mock.count(&Kind::Researcher("C".into())) >= 1);
    let (_, rec) = env.record();
    assert_eq!(rec["agents"].as_array().unwrap().len(), 3);
    assert_eq!(rec["agents"][2]["followup"], true);
    let synth = &env.mock.bodies(&Kind::Synth)[0];
    assert!(synth["input"].to_string().contains("Report 3: C"), "{}", synth["input"]);
    let j: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(j["gap_agents"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gap_round_without_gaps_synthesizes() {
    let env = Env::new(GAP_BASE, |r| normal(r, &["A", "B"])).await;
    let o = run(&env, &["run", "q", "--yes"]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(env.record().1["agents"].as_array().unwrap().len(), 2);
    assert_eq!(env.mock.count(&Kind::Gaps), 1);
    assert_eq!(env.mock.count(&Kind::Synth), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_after_gap_approval_skips_review() {
    let env = Env::new(GAP_BASE, |r| match (&r.kind, r.n) {
        (Kind::Gaps, _) => Reply::Output(vec![gaps(&["C"])]),
        (Kind::Researcher(t), 0) if t == "C" => Reply::Hang,
        _ => normal(r, &["A", "B"]),
    })
    .await;
    let c = Kind::Researcher("C".into());
    let mut child = env.cmd(&["run", "q", "--yes"]).spawn().unwrap();
    until(|| env.mock.count(&c) == 1).await;
    child.kill().await.unwrap();
    let (id, _) = env.record();
    let o = run(&env, &["resume", &id]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(env.mock.count(&Kind::Gaps), 1);
    assert_eq!(env.mock.count(&c), 2);
    assert_eq!(env.record().1["status"], "done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_reports_use_hierarchical_synthesis() {
    let titles = ["A", "B", "C", "D", "E", "F", "G", "H", "I"];
    let env = Env::new("max_parallel = 9\n", move |r| normal(r, &titles)).await;
    let o = run(&env, &["run", "q", "--yes", "--agents", "9"]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(env.mock.count(&Kind::GroupSynth), 2);
    assert_eq!(env.mock.count(&Kind::Synth), 1);
    let synth = env.mock.bodies(&Kind::Synth)[0]["input"].to_string();
    assert!(synth.contains("## Group 1 summary") && synth.contains("## Group 2 summary"), "{synth}");
    let groups: Vec<String> = env.mock.bodies(&Kind::GroupSynth).iter().map(|b| b["input"].to_string()).collect();
    for (i, t) in titles.iter().enumerate() {
        let needle = format!("Report {}: {t}", i + 1);
        assert_eq!(groups.iter().filter(|g| g.contains(&needle)).count(), 1, "{needle}");
    }
    assert!(
        groups.iter().all(|g| !titles.iter().enumerate().all(|(i, t)| g.contains(&format!("Report {}: {t}", i + 1))))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cited_loopback_link_is_not_probed() {
    let host = std::sync::Arc::new(std::sync::OnceLock::<String>::new());
    let h = host.clone();
    let cfg = format!("{BASE}check_links = true\n");
    let env = Env::new(&cfg, move |r| match &r.kind {
        Kind::Researcher(_) => Reply::Output(vec![msg(&format!("[src](http://{}/page)", h.get().unwrap()))]),
        _ => normal(r, &["A", "B"]),
    })
    .await;
    host.set(env.mock.base.trim_start_matches("http://").trim_end_matches("/v1").to_string()).unwrap();
    let o = run(&env, &["run", "q", "--yes", "--json", "--verify"]).await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let (_, rec) = env.record();
    assert_eq!(rec["sources"][0]["status"], "unknown");
    assert_eq!(rec["config"]["verify_citations"], true);
    assert_eq!(env.mock.count(&Kind::Verify), 0);
    let j: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(j["citations"]["links"]["unknown"], 1, "{j}");
    assert_eq!(env.mock.hits(), 1 + env.mock.total());
}
