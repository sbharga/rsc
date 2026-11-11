use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;

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
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_else(|| "page".into())
}

pub fn fetch_http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("rsc/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()
        .expect("fetch client")
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}\n[truncated]", &s[..i]),
        None => s.to_string(),
    }
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let u = url::Url::parse(url).map_err(|e| e.to_string())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("only http/https URLs are supported".into());
    }
    let resp = http.get(u).send().await.map_err(|e| e.without_url().to_string())?;
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
    Ok(truncate_chars(&text, MAX_CHARS))
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
}
