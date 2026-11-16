//! run_js: model-written JavaScript in a fresh headless Chromium-family browser over the DevTools protocol.

use crate::paths;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;
const START_TIMEOUT: Duration = Duration::from_secs(20);
const CMD_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_WAIT: Duration = Duration::from_secs(5);
/// Together these stay under `tools::MAX_CHARS`, so the status line is never cut off.
const MAX_LOG_CHARS: usize = 20_000;
const MAX_RESULT_CHARS: usize = 8_000;
const MAX_ERROR_CHARS: usize = 4_000;

const BROWSER_NAMES: [&str; 8] = [
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "microsoft-edge",
    "microsoft-edge-stable",
    "brave-browser",
    "brave",
];

/// Formats console arguments and the return value the way a developer would want to read them.
const PRELUDE: &str = r#"(() => {
  const norm = (k, x) => typeof x === 'bigint' ? x + 'n' : x instanceof Map || x instanceof Set ? [...x] : ArrayBuffer.isView(x) && !(x instanceof DataView) ? Array.from(x) : x;
  const fmt = (v) => {
    if (typeof v === 'string') return v;
    if (v instanceof Error) return v.stack || String(v);
    if (typeof v === 'bigint') return v + 'n';
    if (v === undefined || typeof v === 'function' || typeof v === 'symbol') return String(v);
    try { const s = JSON.stringify(v, norm); return s === undefined ? String(v) : s; } catch (e) { return String(v); }
  };
  globalThis.__rscFmt = fmt;
  for (const m of ['log', 'info', 'warn', 'error', 'debug']) {
    const orig = console[m].bind(console);
    console[m] = (...a) => orig(a.map(fmt).join(' '));
  }
})()"#;

/// The configured browser, or the first Chrome, Chromium, Edge or Brave found. An explicit setting is never replaced by detection.
pub fn find_browser(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        return p.is_file().then_some(p);
    }
    let mut candidates: Vec<PathBuf> = vec![];
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            let Some(base) = std::env::var_os(var) else { continue };
            for rel in [
                r"Google\Chrome\Application\chrome.exe",
                r"Microsoft\Edge\Application\msedge.exe",
                r"BraveSoftware\Brave-Browser\Application\brave.exe",
                r"Chromium\Application\chrome.exe",
            ] {
                candidates.push(Path::new(&base).join(rel));
            }
        }
    }
    if cfg!(target_os = "macos") {
        let mut roots = vec![PathBuf::from("/Applications")];
        roots.extend(dirs::home_dir().map(|h| h.join("Applications")));
        for root in roots {
            for rel in [
                "Google Chrome.app/Contents/MacOS/Google Chrome",
                "Chromium.app/Contents/MacOS/Chromium",
                "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
                "Brave Browser.app/Contents/MacOS/Brave Browser",
            ] {
                candidates.push(root.join(rel));
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in BROWSER_NAMES {
                candidates.push(dir.join(if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Function definition for the `rsc` namespace.
pub fn tool() -> Value {
    json!({"type":"function","name":"run_js",
        "description":"Run JavaScript in a fresh headless browser page (V8: BigInt, typed arrays, WebAssembly, Web Workers via blob URLs, performance.now). The code is the body of an async function: await works, print with console.log, and return a value to see it. No network, no files, no DOM worth using, and nothing persists between calls. Returns console output, the return value or error, and the run time.",
        "parameters":{"type":"object","properties":{
            "code":{"type":"string","description":"JavaScript run as the body of an async function"},
            "timeout_seconds":{"type":"integer","description":"Wall-clock limit in seconds, 1–300"}},
            "required":["code","timeout_seconds"],"additionalProperties":false},
        "strict":true})
}

fn parse_args(arguments: &str) -> Result<(String, Duration), String> {
    let v: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let code = v["code"].as_str().filter(|c| !c.is_empty()).ok_or("missing code")?;
    let secs = v["timeout_seconds"].as_u64().unwrap_or(DEFAULT_TIMEOUT_SECS).clamp(1, MAX_TIMEOUT_SECS);
    Ok((code.to_string(), Duration::from_secs(secs)))
}

/// Temporary browser profile, deleted on drop.
struct Profile(PathBuf);

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A running browser and its DevTools connection. Field order matters: the process is killed before the profile is removed.
struct Session {
    child: Child,
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next: u64,
    _profile: Profile,
}

fn tail_chars(s: &str, n: usize) -> &str {
    match s.char_indices().rev().nth(n.saturating_sub(1)) {
        Some((i, _)) => &s[i..],
        None => s,
    }
}

async fn launch(browser: &Path) -> Result<Session, String> {
    let dir = std::env::temp_dir().join(format!("rsc-js-{}", uuid::Uuid::new_v4()));
    paths::create_private_dir(&dir).map_err(|e| format!("couldn't create a browser profile: {e}"))?;
    let profile = Profile(dir.clone());
    let mut child = Command::new(browser)
        .args([
            "--headless=new",
            "--remote-debugging-port=0",
            &format!("--user-data-dir={}", dir.display()),
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-sync",
            "--disable-default-apps",
            "--disable-component-update",
            "--disable-background-networking",
            "--disable-background-timer-throttling",
            "--disable-renderer-backgrounding",
            "--disable-backgrounding-occluded-windows",
            "--mute-audio",
            "--proxy-server=127.0.0.1:9",
            "--proxy-bypass-list=<-loopback>",
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
            "about:blank",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("couldn't start the browser: {e}"))?;

    // The browser prints its DevTools URL on stderr; keep draining so the pipe never blocks it.
    let seen: Arc<Mutex<(Option<String>, String)>> = Arc::default();
    if let Some(stderr) = child.stderr.take() {
        let seen = seen.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut g = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some((_, url)) = line.split_once("DevTools listening on ") {
                    g.0 = Some(url.trim().to_string());
                }
                g.1.push_str(&line);
                g.1.push('\n');
                let keep = tail_chars(&g.1, 1000).len();
                let cut = g.1.len() - keep;
                g.1.drain(..cut);
            }
        });
    }

    let deadline = Instant::now() + START_TIMEOUT;
    let url = loop {
        let from_stderr = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0.clone();
        if let Some(u) = from_stderr {
            break u;
        }
        if let Ok(s) = std::fs::read_to_string(dir.join("DevToolsActivePort")) {
            let mut l = s.lines().map(str::trim).filter(|l| !l.is_empty());
            if let (Some(port), Some(path)) = (l.next(), l.next()) {
                break format!("ws://127.0.0.1:{port}{path}");
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            let tail = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1.clone();
            return Err(format!("browser exited ({status}): {}", tail_chars(tail.trim(), 300)));
        }
        if Instant::now() >= deadline {
            return Err("browser did not start within 20 s".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let (ws, _) =
        tokio_tungstenite::connect_async(url).await.map_err(|e| format!("couldn't connect to the browser: {e}"))?;
    Ok(Session { child, ws, next: 0, _profile: profile })
}

impl Session {
    async fn send(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<u64, String> {
        self.next += 1;
        let id = self.next;
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.ws.send(Message::text(msg.to_string())).await.map_err(|_| "browser closed".to_string())?;
        Ok(id)
    }

    async fn recv(&mut self) -> Option<Value> {
        loop {
            match self.ws.next().await? {
                Ok(Message::Text(t)) => {
                    if let Ok(v) = serde_json::from_str(t.as_str()) {
                        return Some(v);
                    }
                }
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    async fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value, String> {
        let id = self.send(method, params, session).await?;
        let wait = async {
            loop {
                let v = self.recv().await.ok_or_else(|| "browser closed".to_string())?;
                if v["id"] == id {
                    return match v["error"]["message"].as_str() {
                        Some(m) => Err(m.to_string()),
                        None => Ok(v["result"].clone()),
                    };
                }
            }
        };
        tokio::time::timeout(CMD_TIMEOUT, wait).await.map_err(|_| format!("{method} timed out"))?
    }

    /// Ask the browser process to quit (it answers even while page JS loops), then kill it if it lingers.
    async fn close(mut self) {
        let _ = self.send("Browser.close", json!({}), None).await;
        if tokio::time::timeout(CLOSE_WAIT, self.child.wait()).await.is_err() {
            let _ = self.child.kill().await;
        }
    }
}

enum End {
    Finished,
    TimedOut(u64),
    Crashed,
}

struct Outcome {
    logs: String,
    logs_truncated: bool,
    result: Option<String>,
    error: Option<String>,
    end: End,
    elapsed: Duration,
}

impl Outcome {
    fn status(&self) -> String {
        let t = self.elapsed.as_secs_f64();
        match &self.end {
            End::Finished if self.error.is_none() => format!("finished in {t:.2} s"),
            End::Finished => format!("threw after {t:.2} s"),
            End::TimedOut(n) => format!("timed out after {n} s; execution stopped, output above is partial"),
            End::Crashed => format!("page crashed after {t:.2} s (out of memory?)"),
        }
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}… [truncated]", &s[..i]),
        None => s.to_string(),
    }
}

fn push_log(o: &mut Outcome, line: &str) {
    if o.logs_truncated {
        return;
    }
    let have = o.logs.chars().count();
    let need = line.chars().count() + 1;
    if have + need > MAX_LOG_CHARS {
        o.logs.extend(line.chars().take(MAX_LOG_CHARS - have));
        o.logs_truncated = true;
    } else {
        o.logs.push_str(line);
        o.logs.push('\n');
    }
}

fn describe(d: &Value) -> String {
    let e = &d["exception"];
    if let Some(s) = e["description"].as_str() {
        return s.to_string();
    }
    if let Some(s) = e["value"].as_str() {
        return s.to_string();
    }
    if !e["value"].is_null() {
        return e["value"].to_string();
    }
    d["text"].as_str().unwrap_or("error").to_string()
}

fn console_line(p: &Value) -> String {
    let args = p["args"].as_array().map(Vec::as_slice).unwrap_or_default();
    let text = args
        .iter()
        .map(|a| {
            if let Some(s) = a["value"].as_str() {
                s.to_string()
            } else if let Some(s) = a["unserializableValue"].as_str() {
                s.to_string()
            } else if !a["value"].is_null() {
                a["value"].to_string()
            } else {
                a["description"].as_str().unwrap_or("").to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let prefix = match p["type"].as_str().unwrap_or("") {
        "error" => "[error] ",
        "warning" => "[warn] ",
        "assert" => "[assert] ",
        _ => "",
    };
    format!("{prefix}{text}")
}

async fn run(browser: &Path, code: &str, timeout: Duration) -> Result<Outcome, String> {
    let mut s = launch(browser).await?;
    match setup(&mut s).await {
        Ok(sid) => Ok(evaluate(s, &sid, code, timeout).await),
        Err(e) => {
            s.close().await;
            Err(e)
        }
    }
}

/// Opens a blank page, attaches, and installs the console formatter; returns the CDP session id.
async fn setup(s: &mut Session) -> Result<String, String> {
    let target = s.call("Target.createTarget", json!({"url":"about:blank"}), None).await?;
    let target_id = target["targetId"].as_str().ok_or("browser setup failed")?.to_string();
    let attached = s.call("Target.attachToTarget", json!({"targetId": target_id, "flatten": true}), None).await?;
    let sid = attached["sessionId"].as_str().ok_or("browser setup failed")?.to_string();
    s.call("Runtime.enable", json!({}), Some(&sid)).await?;
    s.call("Inspector.enable", json!({}), Some(&sid)).await?;
    let r = s.call("Runtime.evaluate", json!({"expression": PRELUDE, "returnByValue": true}), Some(&sid)).await?;
    if !r["exceptionDetails"].is_null() {
        return Err("browser setup failed".into());
    }
    Ok(sid)
}

async fn evaluate(mut s: Session, sid: &str, code: &str, timeout: Duration) -> Outcome {
    let expression = format!(
        "(async () => {{{code}\n}})().then((v) => v === undefined ? undefined : globalThis.__rscFmt(v))\n//# sourceURL=main.js"
    );
    let mut o = Outcome {
        logs: String::new(),
        logs_truncated: false,
        result: None,
        error: None,
        end: End::Crashed,
        elapsed: Duration::ZERO,
    };
    let start = Instant::now();
    let deadline = start + timeout;
    let id = match s
        .send(
            "Runtime.evaluate",
            json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
            Some(sid),
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            o.error = Some(e);
            s.close().await;
            return o;
        }
    };
    o.end = loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break End::TimedOut(timeout.as_secs()),
            msg = s.recv() => {
                let Some(v) = msg else { break End::Crashed };
                if v["id"] == id {
                    if let Some(m) = v["error"]["message"].as_str() {
                        o.error = Some(m.to_string());
                    } else if !v["result"]["exceptionDetails"].is_null() {
                        o.error = Some(describe(&v["result"]["exceptionDetails"]));
                    } else {
                        let val = &v["result"]["result"]["value"];
                        o.result = match val {
                            Value::Null => None,
                            Value::String(t) => Some(t.clone()),
                            other => Some(other.to_string()),
                        };
                    }
                    break End::Finished;
                }
                let p = &v["params"];
                match v["method"].as_str().unwrap_or("") {
                    "Runtime.consoleAPICalled" => push_log(&mut o, &console_line(p)),
                    "Runtime.exceptionThrown" => {
                        let line = format!("[uncaught] {}", describe(&p["exceptionDetails"]));
                        push_log(&mut o, &line);
                    }
                    "Inspector.targetCrashed" | "Target.targetCrashed" | "Target.detachedFromTarget" => {
                        break End::Crashed;
                    }
                    _ => {}
                }
            }
        }
    };
    o.elapsed = start.elapsed();
    s.close().await;
    o
}

fn render(o: &Outcome) -> String {
    let mut s = o.logs.clone();
    if o.logs_truncated {
        s.push_str("\n[console output truncated]\n");
    }
    if let Some(r) = &o.result {
        s.push_str(&format!("[result] {}\n", truncate_chars(r, MAX_RESULT_CHARS)));
    }
    if let Some(e) = &o.error {
        s.push_str(&format!("[error] {}\n", truncate_chars(e, MAX_ERROR_CHARS)));
    }
    if matches!(o.end, End::Finished) && o.logs.is_empty() && o.result.is_none() && o.error.is_none() {
        s.push_str("(nothing printed or returned: use console.log or return)\n");
    }
    s.push_str(&format!("[{}]", o.status()));
    s
}

/// Runs a `run_js` call; returns the model's output and a short status for the timeline.
pub async fn execute(browser: Option<&Path>, arguments: &str) -> (String, String) {
    let Some(browser) = browser else {
        return ("error: run_js is not available".into(), "not available".into());
    };
    let fail = |e: String| (format!("error: {e}"), e);
    let (code, timeout) = match parse_args(arguments) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    match run(browser, &code, timeout).await {
        Ok(o) => (render(&o), o.status()),
        Err(e) => fail(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(end: End, logs: &str) -> Outcome {
        Outcome {
            logs: logs.into(),
            logs_truncated: false,
            result: None,
            error: None,
            end,
            elapsed: Duration::from_millis(1500),
        }
    }

    #[test]
    fn parse_args_clamps_timeout_and_requires_code() {
        assert_eq!(parse_args(r#"{"code":"x","timeout_seconds":0}"#).unwrap().1, Duration::from_secs(1));
        assert_eq!(parse_args(r#"{"code":"x","timeout_seconds":999}"#).unwrap().1, Duration::from_secs(300));
        assert_eq!(parse_args(r#"{"code":"x"}"#).unwrap().1, Duration::from_secs(30));
        assert_eq!(parse_args(r#"{"code":""}"#), Err("missing code".to_string()));
        assert_eq!(parse_args("not json"), Err("missing code".to_string()));
    }

    #[test]
    fn render_timeout_keeps_partial_output_and_status() {
        let s = render(&outcome(End::TimedOut(2), "started\n"));
        assert!(s.starts_with("started\n"));
        assert!(s.ends_with("[timed out after 2 s; execution stopped, output above is partial]"));
    }

    #[test]
    fn render_hints_when_nothing_happened() {
        let s = render(&outcome(End::Finished, ""));
        assert!(s.contains("(nothing printed or returned"));
        assert!(s.ends_with("[finished in 1.50 s]"));
    }

    #[test]
    fn push_log_caps_total_size() {
        let mut o = outcome(End::Finished, "");
        let line = "x".repeat(9_000);
        for _ in 0..4 {
            push_log(&mut o, &line);
        }
        assert!(o.logs_truncated);
        assert_eq!(o.logs.chars().count(), MAX_LOG_CHARS);
        assert!(render(&o).contains("[console output truncated]"));
    }

    #[test]
    fn console_line_prefixes_and_joins_arguments() {
        let p = json!({"type":"error","args":[{"type":"string","value":"a"},{"type":"number","value":2}]});
        assert_eq!(console_line(&p), "[error] a 2");
    }

    fn browser() -> Option<PathBuf> {
        find_browser("")
    }

    #[tokio::test]
    #[ignore = "needs Chrome, Chromium, Edge or Brave"]
    async fn formats_console_and_result() {
        let Some(b) = browser() else { return };
        let (out, _) =
            execute(Some(&b), r#"{"code":"console.log('a', {x: 1n}); return [1,2,3]","timeout_seconds":20}"#).await;
        assert!(out.contains(r#"a {"x":"1n"}"#), "{out}");
        assert!(out.contains("[result] [1,2,3]"), "{out}");
        assert!(out.contains("[finished in"), "{out}");
    }

    #[tokio::test]
    #[ignore = "needs Chrome, Chromium, Edge or Brave"]
    async fn reports_thrown_errors_with_stack() {
        let Some(b) = browser() else { return };
        let (out, _) = execute(Some(&b), r#"{"code":"throw new Error('boom')","timeout_seconds":20}"#).await;
        assert!(out.contains("[error] Error: boom"), "{out}");
        assert!(out.contains("main.js:1"), "{out}");
        assert!(out.contains("[threw after"), "{out}");
    }

    #[tokio::test]
    #[ignore = "needs Chrome, Chromium, Edge or Brave"]
    async fn timeout_stops_infinite_loop() {
        let Some(b) = browser() else { return };
        let t = std::time::Instant::now();
        let (out, _) = execute(
            Some(&b),
            r#"{"code":"console.log('started'); await new Promise(r => setTimeout(r, 50)); while (true) {}","timeout_seconds":2}"#,
        )
        .await;
        assert!(t.elapsed() < Duration::from_secs(15), "took {:?}", t.elapsed());
        assert!(out.contains("started"), "{out}");
        assert!(out.contains("timed out after 2 s"), "{out}");
    }

    #[tokio::test]
    #[ignore = "needs Chrome, Chromium, Edge or Brave"]
    async fn network_is_blocked() {
        let Some(b) = browser() else { return };
        let (out, _) = execute(
            Some(&b),
            r#"{"code":"try { await fetch('http://example.com/', {mode: 'no-cors'}); return 'reached' } catch (e) { return 'blocked' }","timeout_seconds":20}"#,
        )
        .await;
        assert!(out.contains("[result] blocked"), "{out}");
    }
}
