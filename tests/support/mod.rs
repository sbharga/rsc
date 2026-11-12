//! Scripted Responses API mock and a scratch `HOME` for driving the real `rsc` binary.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Planner,
    Researcher(String),
    Compactor,
    Synth,
    GroupSynth,
    Gaps,
    Verify,
}

fn classify(body: &Value) -> Kind {
    match body["text"]["format"]["name"].as_str() {
        Some("plan") => return Kind::Planner,
        Some("gaps") => return Kind::Gaps,
        Some("verify") => return Kind::Verify,
        _ => {}
    }
    let ins = body["instructions"].as_str().unwrap_or("");
    if ins.starts_with("You are one of several researchers") {
        let title = ins.split("Your angle: ").nth(1).and_then(|r| r.split(" — ").next()).unwrap_or("");
        Kind::Researcher(title.to_string())
    } else if ins.starts_with("Merge these researcher reports") {
        Kind::GroupSynth
    } else if ins.starts_with("Several researchers") {
        Kind::Synth
    } else {
        Kind::Compactor
    }
}

pub struct Req<'a> {
    pub kind: Kind,
    /// Number of earlier `/responses` requests of the same kind.
    pub n: usize,
    pub body: &'a Value,
}

pub enum Reply {
    Output(Vec<Value>),
    Error(u16, Value),
    /// Read the request, then never answer.
    Hang,
}

pub fn msg(text: &str) -> Value {
    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text,"annotations":[]}]})
}

pub fn search(q: &str) -> Value {
    json!({"type":"web_search_call","status":"completed","action":{"type":"search","query":q}})
}

pub fn fetch_call(url: &str, call_id: &str) -> Value {
    json!({"type":"function_call","name":"fetch_url","call_id":call_id,"arguments":json!({"url":url}).to_string()})
}

pub fn plan(titles: &[&str]) -> Value {
    let ps: Vec<Value> = titles.iter().map(|t| json!({"title":t,"brief":"b"})).collect();
    msg(&json!({"understanding":"u","notes":"n","questions":[],"perspectives":ps}).to_string())
}

pub fn gaps(titles: &[&str]) -> Value {
    let gs: Vec<Value> = titles.iter().map(|t| json!({"title":t,"brief":"b"})).collect();
    msg(&json!({"notes":"n","gaps":gs}).to_string())
}

type Log = Arc<Mutex<Vec<(Kind, Value)>>>;

pub struct Mock {
    pub base: String,
    log: Log,
    hits: Arc<AtomicUsize>,
}

impl Mock {
    pub async fn start(handler: impl Fn(&Req) -> Reply + Send + Sync + 'static) -> Mock {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let log: Log = Arc::default();
        let hits = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(handler);
        let (l, h) = (log.clone(), hits.clone());
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { return };
                tokio::spawn(serve(sock, handler.clone(), l.clone(), h.clone()));
            }
        });
        Mock { base, log, hits }
    }

    pub fn count(&self, k: &Kind) -> usize {
        self.log.lock().unwrap().iter().filter(|(kind, _)| kind == k).count()
    }

    pub fn bodies(&self, k: &Kind) -> Vec<Value> {
        self.log.lock().unwrap().iter().filter(|(kind, _)| kind == k).map(|(_, b)| b.clone()).collect()
    }

    /// Number of logged `/responses` requests.
    pub fn total(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    /// Every HTTP request, including `/models`.
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

async fn read_request(sock: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
        match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    let len = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while buf.len() < end + len {
        match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    Some((head, buf[end..end + len].to_vec()))
}

fn sse(output: &[Value]) -> String {
    let mut s = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
    let mut ev = |v: Value| s.push_str(&format!("data: {v}\n\n"));
    for it in output {
        if it["type"] == "message" {
            let text = it["content"][0]["text"].as_str().unwrap_or("");
            ev(json!({"type":"response.output_text.delta","delta":text}));
        }
        ev(json!({"type":"response.output_item.done","item":it}));
    }
    ev(json!({"type":"response.completed","response":{"output":output,"usage":{
        "input_tokens":100,"output_tokens":50,
        "input_tokens_details":{"cached_tokens":0},"output_tokens_details":{"reasoning_tokens":0}}}}));
    s
}

fn json_response(status: u16, body: &Value) -> String {
    let body = body.to_string();
    format!(
        "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn serve(
    mut sock: TcpStream,
    handler: Arc<dyn Fn(&Req) -> Reply + Send + Sync>,
    log: Log,
    hits: Arc<AtomicUsize>,
) {
    let Some((head, body)) = read_request(&mut sock).await else { return };
    hits.fetch_add(1, Ordering::SeqCst);
    let path = head.split_whitespace().nth(1).unwrap_or("");
    let resp = if path.ends_with("/models") {
        json_response(
            200,
            &json!({"models":[{"slug":"mock-1","display_name":"Mock","visibility":"list",
                "supported_reasoning_levels":["low","medium","high"]}]}),
        )
    } else if path.ends_with("/responses") {
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let kind = classify(&body);
        let n = {
            let mut l = log.lock().unwrap();
            let n = l.iter().filter(|(k, _)| *k == kind).count();
            l.push((kind.clone(), body.clone()));
            n
        };
        match handler(&Req { kind, n, body: &body }) {
            Reply::Output(items) => sse(&items),
            Reply::Error(status, v) => json_response(status, &v),
            Reply::Hang => std::future::pending().await,
        }
    } else {
        json_response(404, &json!({"error":{"message":"not found"}}))
    };
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
}

pub struct Env {
    pub home: PathBuf,
    pub mock: Mock,
}

impl Env {
    /// `config_toml` is appended to a base config (offline, few turns); keys it sets override the base keys.
    pub async fn new(config_toml: &str, handler: impl Fn(&Req) -> Reply + Send + Sync + 'static) -> Env {
        let mock = Mock::start(handler).await;
        let home = std::env::temp_dir().join(format!("rsc-e2e-{}", uuid::Uuid::new_v4()));
        let root = home.join(".rsc");
        std::fs::create_dir_all(root.join("accounts")).unwrap();
        std::fs::create_dir_all(root.join("research")).unwrap();
        let id: String = Sha256::digest(b"test-client").iter().take(6).map(|b| format!("{b:02x}")).collect();
        let acct = json!({
            "email": "t@example.com", "issuer": "https://issuer.test", "subject": "sub",
            "client_id": "test-client", "ext_agent_host_id": "host",
            "access_token": "tok", "refresh_token": "r", "expires_in": 36000,
            "saved_at": chrono::Utc::now().to_rfc3339(),
            "plan_usage_enabled": true, "welcomed": true,
        });
        std::fs::write(root.join("accounts").join(format!("{id}.json")), acct.to_string()).unwrap();
        std::fs::write(root.join("state.json"), json!({"active_account": id}).to_string()).unwrap();
        let base: String = [("check_updates", "false"), ("max_turns", "5"), ("check_links", "false")]
            .iter()
            .filter(|(k, _)| !config_toml.lines().any(|l| l.trim_start().starts_with(&format!("{k} ="))))
            .map(|(k, v)| format!("{k} = {v}\n"))
            .collect();
        std::fs::write(root.join("config.toml"), format!("{base}{config_toml}")).unwrap();
        Env { home, mock }
    }

    pub fn cmd(&self, args: &[&str]) -> tokio::process::Command {
        let mut c = tokio::process::Command::new(env!("CARGO_BIN_EXE_rsc"));
        c.args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("RSC_API_BASE", &self.mock.base)
            .env("RSC_BACKOFF_MS", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        c
    }

    /// The only research directory: its id and parsed `research.json`; `None` until the run has saved one.
    pub fn try_record(&self) -> Option<(String, Value)> {
        let dir = self.home.join(".rsc/research");
        let ids: Vec<String> =
            std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert!(ids.len() <= 1, "expected one research dir, got {ids:?}");
        let s = std::fs::read_to_string(dir.join(ids.first()?).join("research.json")).ok()?;
        Some((ids[0].clone(), serde_json::from_str(&s).ok()?))
    }

    pub fn record(&self) -> (String, Value) {
        self.try_record().expect("no research.json")
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Poll until `f` holds; panics after 20 s.
pub async fn until(f: impl Fn() -> bool) {
    for _ in 0..1000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("condition not met within 20 s");
}

pub async fn finish(child: tokio::process::Child) -> Output {
    tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .expect("rsc did not exit within 60 s")
        .unwrap()
}

pub fn sigint(pid: u32) {
    let st = std::process::Command::new("kill").args(["-INT", &pid.to_string()]).status().unwrap();
    assert!(st.success());
}
