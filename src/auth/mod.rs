//! Sign in with ChatGPT (SIWC) token-sharing flow.

use crate::paths;
use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

pub const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
pub const REVOKE_URL: &str = "https://auth.openai.com/api/accounts/oauth/revoke";
pub const JWKS_URL: &str = "https://auth.openai.com/.well-known/jwks.json";
pub const ISSUER: &str = "https://auth.openai.com";
pub const RESOURCE: &str = "https://api.openai.com/v1";
pub const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
pub const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
pub const USAGE_URL: &str = "https://chatgpt.com/settings/usage";

#[derive(Debug, Clone)]
pub enum AuthError {
    Declined,
    IncompleteRegistration,
    ClientMismatch,
    IdentityMismatch,
    Retry,
    SignInRequired,
    Cancelled,
    Timeout,
    Retryable(String),
    Fatal(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Declined => write!(f, "Sign-in was declined"),
            AuthError::IncompleteRegistration => write!(f, "Registration did not return a client id"),
            AuthError::ClientMismatch => write!(f, "Sign-in returned a different client id"),
            AuthError::IdentityMismatch => write!(f, "Signed in as a different ChatGPT user"),
            AuthError::Retry => write!(f, "Sign-in code expired"),
            AuthError::SignInRequired => write!(f, "Sign-in required"),
            AuthError::Cancelled => write!(f, "Cancelled"),
            AuthError::Timeout => write!(f, "Sign-in timed out"),
            AuthError::Retryable(m) => write!(f, "Network error: {m}"),
            AuthError::Fatal(m) => write!(f, "{m}"),
        }
    }
}
impl std::error::Error for AuthError {}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    pub issuer: String,
    pub subject: String,
    pub client_id: String,
    pub ext_agent_host_id: String,
    pub id_token: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub token_type: String,
    #[serde(default)]
    pub expires_in: u64,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub saved_at: String,
    #[serde(default)]
    pub plan_usage_enabled: bool,
    #[serde(default)]
    pub welcomed: bool,
}

pub fn account_id(client_id: &str) -> String {
    let d = Sha256::digest(client_id.as_bytes());
    d.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

impl Account {
    pub fn id(&self) -> String {
        account_id(&self.client_id)
    }
    fn path_for(id: &str) -> PathBuf {
        paths::accounts_dir().join(format!("{id}.json"))
    }
    pub fn load(id: &str) -> anyhow::Result<Account> {
        let s = std::fs::read_to_string(Self::path_for(id)).context("read account")?;
        Ok(serde_json::from_str(&s)?)
    }
    pub fn save(&self) -> anyhow::Result<()> {
        paths::atomic_write(&Self::path_for(&self.id()), &serde_json::to_vec_pretty(self)?)
    }
    pub fn has_tokens(&self) -> bool {
        self.refresh_token.is_some()
    }
    fn expires_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let saved = chrono::DateTime::parse_from_rfc3339(&self.saved_at).ok()?;
        Some(saved.with_timezone(&chrono::Utc) + chrono::Duration::seconds(self.expires_in as i64))
    }
    fn stale(&self) -> bool {
        if self.access_token.is_none() {
            return true;
        }
        match self.expires_at() {
            Some(e) => e - chrono::Utc::now() < chrono::Duration::seconds(300),
            None => true,
        }
    }
}

pub fn list_accounts() -> Vec<Account> {
    let mut v: Vec<Account> = std::fs::read_dir(paths::accounts_dir())
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                .filter_map(|s| serde_json::from_str::<Account>(&s).ok())
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| a.email.cmp(&b.email));
    v
}

#[derive(Serialize, Deserialize, Default)]
struct State {
    active_account: Option<String>,
}

pub fn active_account_id() -> Option<String> {
    let s = std::fs::read_to_string(paths::root().join("state.json")).ok()?;
    serde_json::from_str::<State>(&s).ok()?.active_account
}

pub fn set_active_account(id: Option<&str>) -> anyhow::Result<()> {
    let st = State { active_account: id.map(String::from) };
    paths::atomic_write(&paths::root().join("state.json"), &serde_json::to_vec_pretty(&st)?)
}

fn host_id() -> anyhow::Result<String> {
    let p = paths::root().join("host.json");
    if let Ok(s) = std::fs::read_to_string(&p)
        && let Ok(v) = serde_json::from_str::<Value>(&s)
        && let Some(id) = v["ext_agent_host_id"].as_str()
    {
        return Ok(id.to_string());
    }
    let id = format!("urn:uuid:{}", uuid::Uuid::new_v4().hyphenated());
    paths::atomic_write(&p, &serde_json::to_vec_pretty(&serde_json::json!({ "ext_agent_host_id": id }))?)?;
    Ok(id)
}

fn rand_b64(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::fill(&mut b[..]);
    B64.encode(&b)
}

fn net_err(e: reqwest::Error) -> AuthError {
    AuthError::Retryable(e.without_url().to_string())
}

/// Post a form and return (status, json body).
async fn post_form(http: &reqwest::Client, url: &str, form: &[(&str, &str)]) -> Result<(u16, Value), AuthError> {
    let r = http.post(url).form(form).send().await.map_err(net_err)?;
    let status = r.status().as_u16();
    let text = r.text().await.map_err(net_err)?;
    let v = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok((status, v))
}

fn oauth_error(v: &Value) -> String {
    v["error"].as_str().or_else(|| v["error"]["code"].as_str()).unwrap_or("").to_string()
}

const UNUSABLE_REFRESH: &[&str] = &[
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

pub struct SignInUi {
    /// Receives the (redacted) authorize URL to display.
    pub url_tx: mpsc::UnboundedSender<String>,
    pub cancel: CancellationToken,
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(q.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

async fn wait_callback(listener: &TcpListener, state: &str) -> Result<Vec<(String, String)>, AuthError> {
    loop {
        let (mut sock, _) = listener.accept().await.map_err(|e| AuthError::Fatal(e.to_string()))?;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let read = tokio::time::timeout(Duration::from_secs(5), async {
            while buf.len() < 16 * 1024 {
                let n = sock.read(&mut tmp).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        })
        .await;
        if read.is_err() {
            continue;
        }
        let text = String::from_utf8_lossy(&buf);
        let line = text.lines().next().unwrap_or("");
        let mut parts = line.split_whitespace();
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if method == "GET" && path == "/auth/callback" {
            let params = parse_query(query);
            let ok = params.iter().any(|(k, v)| k == "code" && !v.is_empty())
                && params.iter().any(|(k, v)| k == "state" && v == state)
                && !params.iter().any(|(k, _)| k == "error");
            let body = if ok { "Signed in — you can close this tab." } else { "Sign-in failed. Return to rsc." };
            let html = format!(
                "<!doctype html><meta charset=utf-8><title>rsc</title><body style=\"font-family:sans-serif;text-align:center;margin-top:20vh\"><p>{body}</p>"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
            return Ok(params);
        }
        let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    }
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
}

async fn verify_id_token(
    http: &reqwest::Client,
    id_token: &str,
    client_id: &str,
    nonce: &str,
) -> Result<Claims, AuthError> {
    use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
    let header = decode_header(id_token).map_err(|e| AuthError::Fatal(format!("bad id_token: {e}")))?;
    let jwks: JwkSet = http.get(JWKS_URL).send().await.map_err(net_err)?.json().await.map_err(net_err)?;
    let kid = header.kid.ok_or_else(|| AuthError::Fatal("id_token has no kid".into()))?;
    let jwk = jwks.find(&kid).ok_or_else(|| AuthError::Fatal("unknown signing key".into()))?;
    let key = DecodingKey::from_jwk(jwk).map_err(|e| AuthError::Fatal(format!("bad jwk: {e}")))?;
    let mut v = Validation::new(Algorithm::RS256);
    v.set_issuer(&[ISSUER]);
    v.set_audience(&[client_id]);
    v.set_required_spec_claims(&["exp", "iss", "aud"]);
    let data = decode::<Claims>(id_token, &key, &v).map_err(|e| AuthError::Fatal(format!("id_token invalid: {e}")))?;
    if data.claims.nonce.as_deref() != Some(nonce) {
        return Err(AuthError::Fatal("id_token nonce mismatch".into()));
    }
    Ok(data.claims)
}

pub async fn sign_in(existing: Option<&Account>, reconsent: bool, ui: SignInUi) -> Result<Account, AuthError> {
    let http =
        crate::api::http_builder().user_agent(concat!("rsc/", env!("CARGO_PKG_VERSION"))).build().map_err(net_err)?;
    let host = host_id().map_err(|e| AuthError::Fatal(e.to_string()))?;
    paths::ensure_dirs().map_err(|e| AuthError::Fatal(e.to_string()))?;

    let state = rand_b64(32);
    let nonce = rand_b64(32);
    let verifier = rand_b64(64);
    let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));

    let listener = match TcpListener::bind("127.0.0.1:1455").await {
        Ok(l) => l,
        Err(_) => TcpListener::bind("127.0.0.1:0").await.map_err(|e| AuthError::Fatal(e.to_string()))?,
    };
    let port = listener.local_addr().map_err(|e| AuthError::Fatal(e.to_string()))?.port();
    let redirect = format!("http://127.0.0.1:{port}/auth/callback");

    let client_id = existing.map(|a| a.client_id.clone()).unwrap_or_else(|| DYNAMIC_CLIENT.into());
    let mut params: Vec<(&str, String)> = vec![
        ("client_id", client_id.clone()),
        ("ext_agent_host_id", host.clone()),
        ("response_type", "code".into()),
        ("redirect_uri", redirect.clone()),
        ("scope", SCOPE.into()),
        ("resource", RESOURCE.into()),
        ("state", state.clone()),
        ("nonce", nonce.clone()),
        ("code_challenge_method", "S256".into()),
        ("code_challenge", challenge),
    ];
    if existing.is_none() {
        params.push(("agent_name_hint", "rsc".into()));
    }
    if reconsent {
        params.push(("prompt", "consent".into()));
    }
    let shown = url::Url::parse_with_params(AUTHORIZE_URL, &params).map_err(|e| AuthError::Fatal(e.to_string()))?;
    let mut full_params = params.clone();
    if let Some(a) = existing {
        if let Some(t) = &a.id_token {
            full_params.push(("id_token_hint", t.clone()));
        }
        if !a.email.is_empty() {
            full_params.push(("login_hint", a.email.clone()));
        }
    }
    let full = url::Url::parse_with_params(AUTHORIZE_URL, &full_params).map_err(|e| AuthError::Fatal(e.to_string()))?;
    let _ = ui.url_tx.send(shown.to_string());
    let _ = open::that_detached(full.as_str());

    let cb = tokio::select! {
        _ = ui.cancel.cancelled() => return Err(AuthError::Cancelled),
        r = tokio::time::timeout(Duration::from_secs(600), wait_callback(&listener, &state)) => match r {
            Ok(r) => r?,
            Err(_) => return Err(AuthError::Timeout),
        },
    };
    drop(listener);
    let get = |k: &str| cb.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());

    if get("state").as_deref() != Some(state.as_str()) {
        return Err(AuthError::Fatal("state mismatch".into()));
    }
    if get("error").as_deref() == Some("access_denied") {
        return Err(AuthError::Declined);
    }
    if let Some(e) = get("error") {
        return Err(AuthError::Fatal(format!("sign-in error: {e}")));
    }
    let issued_client = match existing {
        None => get("client_id").ok_or(AuthError::IncompleteRegistration)?,
        Some(a) => {
            if let Some(c) = get("client_id")
                && c != a.client_id
            {
                return Err(AuthError::ClientMismatch);
            }
            a.client_id.clone()
        }
    };
    let code = get("code").ok_or_else(|| AuthError::Fatal("callback had no code".into()))?;

    let (status, tok) = post_form(
        &http,
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &issued_client),
            ("code", &code),
            ("code_verifier", &verifier),
            ("redirect_uri", &redirect),
            ("resource", RESOURCE),
        ],
    )
    .await?;
    if status >= 400 {
        let e = oauth_error(&tok);
        if e == "invalid_grant" {
            return Err(AuthError::Retry);
        }
        return Err(AuthError::Fatal(format!("token exchange failed ({status}) {e}")));
    }
    let s = |k: &str| tok[k].as_str().map(String::from);
    let id_token = s("id_token").ok_or_else(|| AuthError::Fatal("no id_token".into()))?;
    let claims = verify_id_token(&http, &id_token, &issued_client, &nonce).await?;
    if let Some(a) = existing
        && !a.subject.is_empty()
        && a.subject != claims.sub
    {
        return Err(AuthError::IdentityMismatch);
    }
    let scopes: Vec<String> = tok["scope"].as_str().unwrap_or("").split_whitespace().map(String::from).collect();
    let acct = Account {
        email: claims.email.or_else(|| existing.map(|a| a.email.clone())).unwrap_or_default(),
        issuer: ISSUER.into(),
        subject: claims.sub,
        client_id: issued_client,
        ext_agent_host_id: host,
        id_token: Some(id_token),
        access_token: s("access_token"),
        refresh_token: s("refresh_token"),
        token_type: s("token_type").unwrap_or_else(|| "Bearer".into()),
        expires_in: tok["expires_in"].as_u64().unwrap_or(3600),
        plan_usage_enabled: scopes.iter().any(|x| x == PLAN_SCOPE),
        scopes,
        saved_at: chrono::Utc::now().to_rfc3339(),
        welcomed: existing.is_some_and(|a| a.welcomed),
    };
    if acct.access_token.is_none() || acct.refresh_token.is_none() {
        return Err(AuthError::Fatal("token response incomplete".into()));
    }
    acct.save().map_err(|e| AuthError::Fatal(e.to_string()))?;
    set_active_account(Some(&acct.id())).map_err(|e| AuthError::Fatal(e.to_string()))?;
    Ok(acct)
}

/// Shared handle that vends fresh access tokens for one account.
pub struct AuthHandle {
    id: String,
    http: reqwest::Client,
    lock: Mutex<()>,
}

impl AuthHandle {
    pub fn new(id: String) -> Arc<AuthHandle> {
        Arc::new(AuthHandle {
            id,
            http: crate::api::http_builder()
                .user_agent(concat!("rsc/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client"),
            lock: Mutex::new(()),
        })
    }

    pub async fn access_token(&self) -> Result<String, AuthError> {
        self.get(false).await
    }

    pub async fn force_refresh(&self) -> Result<String, AuthError> {
        self.get(true).await
    }

    async fn get(&self, force: bool) -> Result<String, AuthError> {
        let acct = Account::load(&self.id).map_err(|_| AuthError::SignInRequired)?;
        if !force
            && !acct.stale()
            && let Some(t) = acct.access_token
        {
            return Ok(t);
        }
        let _g = self.lock.lock().await;
        let lock_path = paths::accounts_dir().join(format!("{}.lock", self.id));
        let file = tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
            let mut o = std::fs::OpenOptions::new();
            o.create(true).truncate(false).write(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
            let f = o.open(lock_path)?;
            f.lock()?;
            Ok(f)
        })
        .await
        .map_err(|e| AuthError::Fatal(e.to_string()))?
        .map_err(|e| AuthError::Fatal(format!("lock: {e}")))?;

        let result = self.refresh_locked(force, &acct).await;
        drop(file);
        result
    }

    async fn refresh_locked(&self, force: bool, before: &Account) -> Result<String, AuthError> {
        let mut acct = Account::load(&self.id).map_err(|_| AuthError::SignInRequired)?;
        // Another process may have refreshed while we waited for the lock.
        let rotated = acct.refresh_token != before.refresh_token || acct.saved_at != before.saved_at;
        if !acct.stale()
            && (!force || rotated)
            && let Some(t) = &acct.access_token
        {
            return Ok(t.clone());
        }
        let rt = acct.refresh_token.clone().ok_or(AuthError::SignInRequired)?;
        let (status, v) = post_form(
            &self.http,
            TOKEN_URL,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &acct.client_id),
                ("refresh_token", &rt),
                ("resource", RESOURCE),
            ],
        )
        .await?;
        if status >= 500 {
            return Err(AuthError::Retryable(format!("token endpoint {status}")));
        }
        if status >= 400 {
            let e = oauth_error(&v);
            if UNUSABLE_REFRESH.contains(&e.as_str()) {
                acct.access_token = None;
                acct.refresh_token = None;
                let _ = acct.save();
                return Err(AuthError::SignInRequired);
            }
            if e == "invalid_client" {
                return Err(AuthError::Fatal("invalid client".into()));
            }
            return Err(AuthError::Fatal(format!("refresh failed ({status}) {e}")));
        }
        let at = v["access_token"]
            .as_str()
            .ok_or_else(|| AuthError::Fatal("refresh returned no access_token".into()))?
            .to_string();
        acct.access_token = Some(at.clone());
        if let Some(r) = v["refresh_token"].as_str() {
            acct.refresh_token = Some(r.to_string());
        }
        if let Some(i) = v["id_token"].as_str() {
            acct.id_token = Some(i.to_string());
        }
        acct.expires_in = v["expires_in"].as_u64().unwrap_or(3600);
        if let Some(sc) = v["scope"].as_str() {
            acct.scopes = sc.split_whitespace().map(String::from).collect();
            acct.plan_usage_enabled = acct.scopes.iter().any(|x| x == PLAN_SCOPE);
        }
        acct.saved_at = chrono::Utc::now().to_rfc3339();
        acct.save().map_err(|e| AuthError::Fatal(e.to_string()))?;
        Ok(at)
    }
}

/// Revoke the refresh token and null local tokens. Returns whether remote revocation was confirmed.
pub async fn sign_out(id: &str) -> bool {
    let Ok(mut acct) = Account::load(id) else { return false };
    let http = crate::api::http_builder().build().expect("http client");
    let mut confirmed = acct.refresh_token.is_none();
    if let Some(rt) = acct.refresh_token.clone() {
        for (i, delay) in [0u64, 1, 2, 4].iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(Duration::from_secs(*delay)).await;
            }
            match post_form(
                &http,
                REVOKE_URL,
                &[("token", &rt), ("token_type_hint", "refresh_token"), ("client_id", &acct.client_id)],
            )
            .await
            {
                Ok((s, _)) if s < 300 => {
                    confirmed = true;
                    break;
                }
                Ok((s, _)) if s < 500 => break,
                _ => {}
            }
        }
    }
    acct.access_token = None;
    acct.refresh_token = None;
    acct.id_token = None;
    let _ = acct.save();
    if active_account_id().as_deref() == Some(id) {
        let _ = set_active_account(None);
    }
    confirmed
}
