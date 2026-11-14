use super::store::LinkStatus;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

const MAX_REDIRECTS: usize = 5;
const MAX_BODY: usize = 5 * 1024 * 1024;
const MAX_CHARS: usize = 30_000;

pub fn web_search_tool() -> Value {
    json!({"type": "web_search"})
}

pub fn fetch_url_namespace() -> Value {
    json!({"type":"namespace","name":"rsc","description":"Local helpers","tools":[
        {"type":"function","name":"fetch_url",
         "description":"Fetch a web page or PDF and return its readable text.",
         "parameters":{"type":"object","properties":{"url":{"type":"string"}},
                       "required":["url"],"additionalProperties":false},
         "strict":true}]})
}

pub fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_else(|| "page".into())
}

const BLOCKED_MSG: &str = "blocked address";

#[derive(Debug)]
struct Blocked;

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(BLOCKED_MSG)
    }
}

impl std::error::Error for Blocked {}

/// True for addresses `fetch_url` must never reach: loopback, private, link-local, CGNAT, multicast, reserved.
pub fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            o[0] == 0
                || o[0] == 10
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || o[0] == 127
                || (o[0] == 169 && o[1] == 254)
                || (o[0] == 172 && (16..32).contains(&o[1]))
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                || o[0] >= 224
        }
        IpAddr::V6(a) => {
            if a.is_unspecified() || a.is_loopback() {
                return true;
            }
            if let Some(v4) = a.to_ipv4() {
                return is_blocked(IpAddr::V4(v4));
            }
            let s = a.segments();
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let o = a.octets();
                return is_blocked(IpAddr::V4(Ipv4Addr::new(o[12], o[13], o[14], o[15])));
            }
            (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xffc0) == 0xfec0
                || (s[0] & 0xff00) == 0xff00
        }
    }
}

/// IP-literal hosts bypass the DNS resolver, so they are checked on the URL itself.
fn url_blocked(u: &url::Url) -> bool {
    match u.host() {
        Some(url::Host::Ipv4(a)) => is_blocked(a.into()),
        Some(url::Host::Ipv6(a)) => is_blocked(a.into()),
        _ => false,
    }
}

/// Rejects any name that resolves to a blocked address.
struct GuardedResolver;

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if addrs.iter().any(|a| is_blocked(a.ip())) {
                return Err(Box::new(Blocked) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn fetch_builder() -> reqwest::ClientBuilder {
    crate::api::http_builder()
        .user_agent(concat!("rsc/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .no_proxy()
        .dns_resolver(Arc::new(GuardedResolver))
        .redirect(reqwest::redirect::Policy::custom(|a| {
            if a.previous().len() > MAX_REDIRECTS {
                a.error("too many redirects")
            } else if url_blocked(a.url()) {
                a.error(Blocked)
            } else {
                a.follow()
            }
        }))
}

pub fn fetch_http() -> reqwest::Client {
    fetch_builder().build().expect("fetch client")
}

fn is_blocked_err(e: &reqwest::Error) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if err.is::<Blocked>() {
            return true;
        }
        cur = err.source();
    }
    false
}

/// Frame fetched page text so the model can tell it from instructions; the nonce keeps pages from forging the closing tag.
fn wrap_untrusted(nonce: &str, url: &str, text: &str) -> String {
    format!("<fetched_{nonce} url=\"{url}\">\n{text}\n</fetched_{nonce}>")
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}\n[truncated]", &s[..i]),
        None => s.to_string(),
    }
}

pub async fn fetch(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let u = url::Url::parse(url).map_err(|e| e.to_string())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("only http/https URLs are supported".into());
    }
    if url_blocked(&u) {
        return Err(BLOCKED_MSG.into());
    }
    let final_url = u.as_str().to_string();
    let resp = http
        .get(u)
        .send()
        .await
        .map_err(|e| if is_blocked_err(&e) { BLOCKED_MSG.to_string() } else { e.without_url().to_string() })?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mut body: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.without_url().to_string())?;
        body.extend_from_slice(&chunk);
        if body.len() >= MAX_BODY {
            body.truncate(MAX_BODY);
            break;
        }
    }
    let text = if ct == "text/html" || ct == "application/xhtml+xml" {
        html2text::from_read(&body[..], 100).map_err(|e| e.to_string())?
    } else if ct == "application/pdf" {
        tokio::task::spawn_blocking(move || pdf_extract::extract_text_from_mem(&body))
            .await
            .map_err(|_| "pdf extraction failed".to_string())?
            .map_err(|e| e.to_string())?
    } else if ct.starts_with("text/") {
        String::from_utf8_lossy(&body).into_owned()
    } else {
        return Ok(format!("unsupported content-type {ct}"));
    };
    let mut b = [0u8; 4];
    rand::fill(&mut b[..]);
    let nonce = format!("{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3]);
    Ok(wrap_untrusted(&nonce, &final_url, &truncate_chars(&text, MAX_CHARS)))
}

fn classify_status(code: u16) -> Option<LinkStatus> {
    match code {
        200..=299 => Some(LinkStatus::Live),
        404 | 410 => Some(LinkStatus::Dead),
        _ => None,
    }
}

/// Is `url` still served? HEAD first, then GET (status only, the body is never read). Blocked addresses and
/// anything inconclusive are `Unknown`.
pub async fn probe(http: &reqwest::Client, url: &str) -> LinkStatus {
    let Ok(u) = url::Url::parse(url) else { return LinkStatus::Unknown };
    if !matches!(u.scheme(), "http" | "https") || url_blocked(&u) {
        return LinkStatus::Unknown;
    }
    match http.head(u.clone()).send().await {
        Ok(r) => {
            if let Some(s) = classify_status(r.status().as_u16()) {
                return s;
            }
        }
        Err(e) if is_blocked_err(&e) => return LinkStatus::Unknown,
        Err(_) => {}
    }
    match http.get(u).send().await {
        Ok(r) => classify_status(r.status().as_u16()).unwrap_or(LinkStatus::Unknown),
        Err(_) => LinkStatus::Unknown,
    }
}

/// Execute a model-issued function call; the result is always a string for the model.
pub async fn execute(http: &reqwest::Client, name: &str, arguments: &str) -> String {
    if name != "fetch_url" {
        return "error: unknown tool".into();
    }
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let Some(url) = args["url"].as_str() else {
        return "error: missing url".into();
    };
    match fetch(http, url).await {
        Ok(t) => t,
        Err(e) => format!("error: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation() {
        assert_eq!(truncate_chars("abc", 5), "abc");
        assert_eq!(truncate_chars("héllo", 2), "hé\n[truncated]");
    }

    #[tokio::test]
    async fn rejects_non_http() {
        let h = fetch_http();
        let out = execute(&h, "fetch_url", r#"{"url":"file:///etc/passwd"}"#).await;
        assert!(out.starts_with("error:"), "{out}");
        assert_eq!(execute(&h, "other", "{}").await, "error: unknown tool");
    }

    #[test]
    fn blocked_ranges() {
        let cases = [
            ("127.0.0.1", true),
            ("10.1.2.3", true),
            ("172.16.0.1", true),
            ("172.31.255.255", true),
            ("192.168.1.1", true),
            ("169.254.169.254", true),
            ("100.64.0.1", true),
            ("0.0.0.0", true),
            ("224.0.0.1", true),
            ("255.255.255.255", true),
            ("::1", true),
            ("::", true),
            ("fd00::1", true),
            ("fe80::1", true),
            ("::ffff:127.0.0.1", true),
            ("::ffff:169.254.169.254", true),
            ("64:ff9b::a9fe:a9fe", true),
            ("8.8.8.8", false),
            ("1.1.1.1", false),
            ("172.32.0.1", false),
            ("100.128.0.1", false),
            ("2606:4700::1111", false),
            ("::ffff:8.8.8.8", false),
        ];
        for (ip, want) in cases {
            assert_eq!(is_blocked(ip.parse().unwrap()), want, "{ip}");
        }
    }

    #[tokio::test]
    async fn blocks_ip_literal() {
        let h = fetch_http();
        for url in ["http://127.0.0.1:9/", "http://[::1]:9/", "http://0x7f.1:9/"] {
            let args = json!({ "url": url }).to_string();
            assert_eq!(execute(&h, "fetch_url", &args).await, "error: blocked address", "{url}");
        }
    }

    async fn serve() -> SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let req = String::from_utf8_lossy(&buf);
                    let mut words = req.split_whitespace();
                    let head = words.next() == Some("HEAD");
                    let path = words.next().unwrap_or("/").to_string();
                    let port = addr.port();
                    let redirect = |to: String| format!("302 Found\r\nLocation: {to}\r\n");
                    let (status, body) = match path.as_str() {
                        "/ok" => ("200 OK\r\nContent-Type: text/plain\r\n".to_string(), "hello"),
                        "/to-ip" => (redirect(format!("http://127.0.0.1:{port}/ok")), ""),
                        "/to-localhost" => (redirect(format!("http://localhost:{port}/ok")), ""),
                        "/loop" => (redirect("/loop".into()), ""),
                        "/gone" => ("404 Not Found\r\n".to_string(), ""),
                        "/nohead" if head => ("405 Method Not Allowed\r\n".to_string(), ""),
                        "/nohead" => ("200 OK\r\nContent-Type: text/plain\r\n".to_string(), "hello"),
                        _ => ("404 Not Found\r\n".to_string(), ""),
                    };
                    let resp =
                        format!("HTTP/1.1 {status}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn blocks_redirects_to_private() {
        let addr = serve().await;
        let port = addr.port();
        let h = fetch_builder().resolve("public.test", addr).build().unwrap();
        let get = |path: &str| {
            let args = json!({ "url": format!("http://public.test:{port}{path}") }).to_string();
            let h = h.clone();
            async move { execute(&h, "fetch_url", &args).await }
        };
        let ok = get("/ok").await;
        assert!(ok.starts_with("<fetched_") && ok.contains("hello"), "{ok}");
        assert_eq!(get("/to-localhost").await, "error: blocked address");
        assert_eq!(get("/to-ip").await, "error: blocked address");
        let looped = get("/loop").await;
        assert!(looped.starts_with("error:") && looped != "error: blocked address", "{looped}");
    }

    #[tokio::test]
    async fn probe_classifies() {
        let addr = serve().await;
        let port = addr.port();
        let h = fetch_builder().resolve("public.test", addr).build().unwrap();
        let url = |p: &str| format!("http://public.test:{port}{p}");
        assert_eq!(probe(&h, &url("/ok")).await, LinkStatus::Live);
        assert_eq!(probe(&h, &url("/gone")).await, LinkStatus::Dead);
        assert_eq!(probe(&h, &url("/nohead")).await, LinkStatus::Live);
        assert_eq!(probe(&h, &url("/to-localhost")).await, LinkStatus::Unknown);
        assert_eq!(probe(&h, "http://127.0.0.1:9/").await, LinkStatus::Unknown);
    }
}
