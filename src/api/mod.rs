//! Responses API client (ChatGPT-plan token sharing).

pub mod sse;

use crate::auth::{AuthError, AuthHandle};
use crate::pricing::Usage;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

const API_BASE: &str = "https://api.openai.com/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrKind {
    Retryable,
    UsageLimit,
    Auth,
    Fatal,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct ApiError {
    pub kind: ErrKind,
    pub status: Option<u16>,
    pub code: Option<String>,
    pub param: Option<String>,
    pub message: String,
    pub request_id: Option<String>,
}

impl ApiError {
    pub fn new(kind: ErrKind, message: impl Into<String>) -> Self {
        Self { kind, status: None, code: None, param: None, message: message.into(), request_id: None }
    }
    pub fn cancelled() -> Self {
        Self::new(ErrKind::Cancelled, "Cancelled")
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ApiError {}

/// Classify an error response/event. `status` is None for in-stream failures.
pub fn classify(status: Option<u16>, body: &Value, request_id: Option<String>) -> ApiError {
    let err = if body["error"].is_object() {
        &body["error"]
    } else if body["response"]["error"].is_object() {
        &body["response"]["error"]
    } else {
        body
    };
    let code = err["code"].as_str().filter(|s| !s.is_empty()).map(String::from);
    let param = err["param"].as_str().map(String::from);
    let message = err["message"]
        .as_str()
        .or_else(|| body["detail"].as_str())
        .or_else(|| body["error"].as_str())
        .unwrap_or("request failed")
        .to_string();
    let c = code.as_deref().unwrap_or("");
    let kind = match c {
        "subscription_sharing_usage_limit_exceeded" => ErrKind::UsageLimit,
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => ErrKind::Retryable,
        "subscription_sharing_invalid_user" => ErrKind::Fatal,
        _ => match status {
            Some(401) => ErrKind::Auth,
            Some(s) if s >= 500 => ErrKind::Retryable,
            _ => ErrKind::Fatal,
        },
    };
    ApiError { kind, status, code, param, message, request_id }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        let kind = match e {
            AuthError::Retryable(_) => ErrKind::Retryable,
            AuthError::SignInRequired => ErrKind::Auth,
            _ => ErrKind::Fatal,
        };
        ApiError::new(kind, e.to_string())
    }
}

#[derive(Clone, Debug)]
pub enum StreamDelta {
    Text(String),
    Item(Value),
    Restart,
    /// Reasoning-summary text delta.
    Reasoning(String),
    /// A new reasoning-summary part started.
    ReasoningBreak,
}

#[derive(Clone, Debug, Default)]
pub struct ResponseRequest {
    pub model: String,
    pub instructions: String,
    pub input: Vec<Value>,
    pub tools: Vec<Value>,
    pub effort: Option<String>,
    pub text: Option<Value>,
    pub context_management: Option<Vec<Value>>,
    pub reasoning_summary: bool,
}

impl ResponseRequest {
    pub fn body(&self) -> Value {
        let mut b = json!({
            "model": self.model,
            "instructions": self.instructions,
            "input": self.input,
            "tools": self.tools,
            "store": false,
            "stream": true,
        });
        let mut reasoning = serde_json::Map::new();
        if let Some(e) = &self.effort
            && e != "default"
        {
            reasoning.insert("effort".into(), json!(e));
            if self.reasoning_summary {
                reasoning.insert("summary".into(), json!("auto"));
            }
        }
        if !reasoning.is_empty() {
            b["reasoning"] = Value::Object(reasoning);
        }
        if let Some(t) = &self.text {
            b["text"] = t.clone();
        }
        if let Some(c) = &self.context_management {
            b["context_management"] = json!(c);
        }
        b
    }
}

#[derive(Clone, Debug)]
pub struct Completed {
    pub output: Vec<Value>,
    pub usage: Usage,
}

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub slug: String,
    pub display_name: String,
    pub efforts: Vec<String>,
}

pub fn parse_usage(resp: &Value, output: &[Value]) -> Usage {
    let u = &resp["usage"];
    Usage {
        input: u["input_tokens"].as_u64().unwrap_or(0),
        cached: u["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0),
        output: u["output_tokens"].as_u64().unwrap_or(0),
        reasoning: u["output_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0),
        web_search_calls: output
            .iter()
            .filter(|i| i["type"] == "web_search_call" && i["action"]["type"] == "search")
            .count() as u64,
    }
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    auth: Arc<AuthHandle>,
}

impl Client {
    pub fn new(auth: Arc<AuthHandle>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("rsc/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .expect("http client");
        Self { http, auth }
    }

    fn net(e: reqwest::Error) -> ApiError {
        ApiError::new(ErrKind::Retryable, e.without_url().to_string())
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, ApiError> {
        let mut token = self.auth.access_token().await?;
        let mut refreshed = false;
        loop {
            let r = self
                .http
                .get(format!("{API_BASE}/models"))
                .bearer_auth(&token)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(Self::net)?;
            let status = r.status().as_u16();
            let rid = r.headers().get("x-request-id").and_then(|v| v.to_str().ok()).map(String::from);
            let body: Value = r.json().await.unwrap_or(Value::Null);
            if status == 401 && !refreshed {
                refreshed = true;
                token = self.auth.force_refresh().await?;
                continue;
            }
            if status >= 400 {
                return Err(classify(Some(status), &body, rid));
            }
            return Ok(parse_models(&body));
        }
    }

    async fn stream_once(
        &self,
        req: &ResponseRequest,
        tx: Option<&UnboundedSender<StreamDelta>>,
        cancel: &CancellationToken,
        token: &str,
    ) -> Result<Completed, ApiError> {
        let send = self
            .http
            .post(format!("{API_BASE}/responses"))
            .bearer_auth(token)
            .json(&req.body())
            .send();
        let resp = tokio::select! {
            _ = cancel.cancelled() => return Err(ApiError::cancelled()),
            r = send => r.map_err(Self::net)?,
        };
        let status = resp.status().as_u16();
        let rid = resp.headers().get("x-request-id").and_then(|v| v.to_str().ok()).map(String::from);
        if status >= 400 {
            let text = resp.text().await.unwrap_or_default();
            let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let mut e = classify(Some(status), &body, rid);
            if body.is_null() && !text.is_empty() {
                e.message = text.chars().take(300).collect();
            }
            return Err(e);
        }
        let mut stream = resp.bytes_stream();
        let mut parser = sse::SseParser::default();
        let mut items: Vec<Value> = Vec::new();
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(ApiError::cancelled()),
                c = stream.next() => c,
            };
            let Some(chunk) = chunk else {
                let mut e = ApiError::new(ErrKind::Retryable, "stream ended before completion");
                e.request_id = rid;
                return Err(e);
            };
            let chunk = chunk.map_err(|e| {
                let mut a = Self::net(e);
                a.request_id = rid.clone();
                a
            })?;
            for ev in parser.push(&chunk) {
                match ev["type"].as_str().unwrap_or("") {
                    "response.reasoning_summary_text.delta" => {
                        if let (Some(tx), Some(d)) = (tx, ev["delta"].as_str()) {
                            let _ = tx.send(StreamDelta::Reasoning(d.to_string()));
                        }
                    }
                    "response.reasoning_summary_part.added" => {
                        if let Some(tx) = tx {
                            let _ = tx.send(StreamDelta::ReasoningBreak);
                        }
                    }
                    "response.output_text.delta" => {
                        if let (Some(tx), Some(d)) = (tx, ev["delta"].as_str()) {
                            let _ = tx.send(StreamDelta::Text(d.to_string()));
                        }
                    }
                    "response.output_item.done" => {
                        items.push(ev["item"].clone());
                        if let Some(tx) = tx {
                            let _ = tx.send(StreamDelta::Item(ev["item"].clone()));
                        }
                    }
                    "response.completed" => {
                        let resp = &ev["response"];
                        let mut output: Vec<Value> = resp["output"].as_array().cloned().unwrap_or_default();
                        if output.is_empty() {
                            output = std::mem::take(&mut items);
                        }
                        let usage = parse_usage(resp, &output);
                        return Ok(Completed { output, usage });
                    }
                    "response.failed" => return Err(classify(None, &ev, rid)),
                    "response.incomplete" => {
                        let reason = ev["response"]["incomplete_details"]["reason"].as_str().unwrap_or("unknown");
                        let mut e = ApiError::new(ErrKind::Fatal, format!("response incomplete: {reason}"));
                        e.request_id = rid;
                        return Err(e);
                    }
                    "error" => return Err(classify(None, &ev, rid)),
                    _ => {}
                }
            }
        }
    }

    pub async fn call_with_retry(
        &self,
        req: &ResponseRequest,
        tx: Option<&UnboundedSender<StreamDelta>>,
        cancel: &CancellationToken,
    ) -> Result<Completed, ApiError> {
        let mut retries = 0usize;
        let mut refreshed = false;
        let mut token = self.auth.access_token().await?;
        loop {
            match self.stream_once(req, tx, cancel, &token).await {
                Ok(c) => return Ok(c),
                Err(e) => match e.kind {
                    ErrKind::Retryable if retries < 3 => {
                        let delay = 2u64 << retries;
                        retries += 1;
                        if let Some(tx) = tx {
                            let _ = tx.send(StreamDelta::Restart);
                        }
                        tokio::select! {
                            _ = cancel.cancelled() => return Err(ApiError::cancelled()),
                            _ = tokio::time::sleep(Duration::from_secs(delay)) => {}
                        }
                        // token may have expired during a long outage
                        token = self.auth.access_token().await?;
                    }
                    ErrKind::Auth if !refreshed => {
                        refreshed = true;
                        if let Some(tx) = tx {
                            let _ = tx.send(StreamDelta::Restart);
                        }
                        token = self.auth.force_refresh().await?;
                    }
                    _ => return Err(e),
                },
            }
        }
    }
}

pub fn parse_models(body: &Value) -> Vec<ModelInfo> {
    body["models"]
        .as_array()
        .or_else(|| body["data"].as_array())
        .map(|a| {
            a.iter()
                .filter(|m| m["visibility"] == "list")
                .filter_map(|m| {
                    let slug = m["slug"].as_str().or_else(|| m["id"].as_str())?.to_string();
                    let display_name = m["display_name"].as_str().unwrap_or(&slug).to_string();
                    let mut efforts: Vec<String> = m["supported_reasoning_levels"]
                        .as_array()
                        .map(|l| {
                            l.iter()
                                .filter_map(|e| e.as_str().or_else(|| e["effort"].as_str()).map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    if efforts.is_empty() {
                        efforts = ["low", "medium", "high", "xhigh"].iter().map(|s| s.to_string()).collect();
                    }
                    Some(ModelInfo { slug, display_name, efforts })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_limit() {
        let b = json!({"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"m"}});
        let e = classify(Some(429), &b, None);
        assert_eq!(e.kind, ErrKind::UsageLimit);
    }

    #[test]
    fn detail_503() {
        let e = classify(Some(503), &json!({"detail":"x"}), Some("r1".into()));
        assert_eq!(e.kind, ErrKind::Retryable);
        assert_eq!(e.message, "x");
        assert_eq!(e.request_id.as_deref(), Some("r1"));
    }

    #[test]
    fn unsupported_capability_keeps_param() {
        let b = json!({"error":{"code":"subscription_sharing_unsupported_capability","param":"context_management","message":"no"}});
        let e = classify(Some(400), &b, None);
        assert_eq!(e.kind, ErrKind::Fatal);
        assert_eq!(e.param.as_deref(), Some("context_management"));
    }

    #[test]
    fn failed_event_usage_limit_and_401() {
        let ev = json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"m"}}});
        assert_eq!(classify(None, &ev, None).kind, ErrKind::UsageLimit);
        assert_eq!(classify(Some(401), &json!({}), None).kind, ErrKind::Auth);
        let inv = json!({"error":{"code":"subscription_sharing_invalid_user"}});
        assert_eq!(classify(Some(401), &inv, None).kind, ErrKind::Fatal);
    }

    #[test]
    fn request_body_omits_forbidden_fields() {
        let r = ResponseRequest { model: "m".into(), effort: Some("default".into()), ..Default::default() };
        let b = r.body();
        assert!(b.get("reasoning").is_none());
        assert_eq!(b["store"], false);
        assert_eq!(b["stream"], true);
        assert!(b.get("max_output_tokens").is_none());
    }

    #[test]
    fn reasoning_summary_requires_effort() {
        let r = ResponseRequest {
            model: "m".into(),
            effort: Some("high".into()),
            reasoning_summary: true,
            ..Default::default()
        };
        assert_eq!(r.body()["reasoning"], json!({"effort":"high","summary":"auto"}));
        let r = ResponseRequest { effort: Some("default".into()), reasoning_summary: true, ..r };
        assert!(r.body().get("reasoning").is_none());
    }

    #[test]
    fn models_parse() {
        let b = json!({"models":[
            {"slug":"a","display_name":"A","visibility":"list","supported_reasoning_levels":[{"effort":"low"},"high"]},
            {"slug":"b","visibility":"hide"},
            {"slug":"c","visibility":"list"}]});
        let m = parse_models(&b);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].efforts, vec!["low", "high"]);
        assert_eq!(m[1].display_name, "c");
        assert_eq!(m[1].efforts.len(), 4);
    }
}
