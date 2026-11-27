//! Twitch sign-in (device code flow, no client secret) and a Helix client that refreshes its
//! token once when Twitch says it expired.
//!
//! References: dev.twitch.tv/docs/authentication/getting-tokens-oauth (device code grant),
//! dev.twitch.tv/docs/authentication/refresh-tokens, dev.twitch.tv/docs/authentication/validate-tokens.

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub const ID_BASE: &str = "https://id.twitch.tv";
pub const API_BASE: &str = "https://api.twitch.tv/helix";

#[derive(Debug)]
pub enum Error {
    /// The token was rejected (HTTP 401 or 403).
    Unauthorized { status: u16, body: String },
    Http { status: u16, body: String },
    Network(reqwest::Error),
    Decode(serde_json::Error),
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unauthorized { status, body } => write!(f, "the token was rejected ({status}): {body}"),
            Error::Http { status, body } => write!(f, "{}", helix_message(body).unwrap_or_else(|| format!("request failed with HTTP {status}: {body}"))),
            Error::Network(e) => write!(f, "network error: {e}"),
            Error::Decode(e) => write!(f, "could not read the response: {e}"),
            Error::Other(s) => write!(f, "{s}"),
        }
    }
}

/// Helix errors carry a readable `message`, like "The user specified in the user_id field is already banned."
fn helix_message(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("message").and_then(Value::as_str).filter(|m| !m.is_empty()).map(str::to_string)
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Network(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Decode(e)
    }
}

impl Error {
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Error::Unauthorized { .. })
    }
}

pub type Result<T> = std::result::Result<T, Error>;

async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let code = status.as_u16();
    let body: String = resp.text().await.unwrap_or_default().chars().take(500).collect();
    if code == 401 {
        Err(Error::Unauthorized { status: code, body })
    } else {
        Err(Error::Http { status: code, body })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TwitchTokens {
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub user_id: String,
    pub login: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ValidateResponse {
    user_id: Option<String>,
    login: Option<String>,
}

pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("SylvaChat/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()?)
}

/// Step 1 of sign-in: ask Twitch for a code the user enters at `verification_uri`.
pub async fn start_device_flow(http: &reqwest::Client, client_id: &str, scopes: &str) -> Result<DeviceCode> {
    let resp = http
        .post(format!("{ID_BASE}/oauth2/device"))
        .form(&[("client_id", client_id), ("scopes", scopes)])
        .send()
        .await?;
    Ok(check(resp).await?.json().await?)
}

/// Step 2: poll until the user approves, the code expires, or `cancel` fires.
pub async fn finish_device_flow(
    http: &reqwest::Client,
    client_id: &str,
    scopes: &str,
    code: &DeviceCode,
    cancel: &CancellationToken,
) -> Result<TwitchTokens> {
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let interval = Duration::from_secs(code.interval.max(1));
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Other("the Twitch sign-in code expired, please try again".into()));
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Other("Twitch sign-in was cancelled".into())),
            _ = tokio::time::sleep(interval) => {}
        }
        let resp = http
            .post(format!("{ID_BASE}/oauth2/token"))
            .form(&[
                ("client_id", client_id),
                ("scopes", scopes),
                ("device_code", code.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?;
        if resp.status().as_u16() == 400 {
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            if body.get("message").and_then(Value::as_str) == Some("authorization_pending") {
                continue;
            }
            return Err(Error::Http { status: 400, body: body.to_string() });
        }
        let tok: TokenResponse = check(resp).await?.json().await?;
        let (user_id, login) = validate(http, &tok.access_token).await?;
        return Ok(TwitchTokens {
            client_id: client_id.to_string(),
            access_token: tok.access_token,
            refresh_token: tok.refresh_token,
            user_id,
            login,
        });
    }
}

/// `GET /oauth2/validate`: returns `(user_id, login)` for a user token.
async fn validate(http: &reqwest::Client, access_token: &str) -> Result<(String, String)> {
    let resp = http
        .get(format!("{ID_BASE}/oauth2/validate"))
        .header("Authorization", format!("OAuth {access_token}"))
        .send()
        .await?;
    let v: ValidateResponse = check(resp).await?.json().await?;
    match (v.user_id, v.login) {
        (Some(id), Some(login)) => Ok((id, login)),
        _ => Err(Error::Other("Twitch returned an app token; a user token is required".into())),
    }
}

type RefreshHook = Box<dyn Fn(&TwitchTokens) + Send + Sync>;

/// Helix client that refreshes its token once on a 401 and reports new tokens via `on_refresh`.
pub struct TwitchClient {
    http: reqwest::Client,
    tokens: Mutex<TwitchTokens>,
    on_refresh: Option<RefreshHook>,
}

impl TwitchClient {
    pub fn new(tokens: TwitchTokens, on_refresh: Option<RefreshHook>) -> Result<Arc<Self>> {
        Ok(Arc::new(Self { http: http_client()?, tokens: Mutex::new(tokens), on_refresh }))
    }

    pub async fn tokens(&self) -> TwitchTokens {
        self.tokens.lock().await.clone()
    }

    async fn refresh(&self) -> Result<()> {
        let mut t = self.tokens.lock().await;
        let Some(rt) = t.refresh_token.clone() else {
            return Err(Error::Unauthorized { status: 401, body: "Twitch session expired".into() });
        };
        let resp = self
            .http
            .post(format!("{ID_BASE}/oauth2/token"))
            .form(&[
                ("client_id", t.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", rt.as_str()),
            ])
            .send()
            .await?;
        let status = resp.status().as_u16();
        if status == 400 || status == 401 {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Unauthorized { status, body });
        }
        let tok: TokenResponse = check(resp).await?.json().await?;
        t.access_token = tok.access_token;
        if tok.refresh_token.is_some() {
            t.refresh_token = tok.refresh_token;
        }
        if let Some(hook) = &self.on_refresh {
            hook(&t);
        }
        Ok(())
    }

    /// Any Helix call. Returns the JSON body, or `null` for an empty one (204).
    pub async fn helix(&self, method: Method, path: &str, query: &[(&str, String)], body: Option<&Value>) -> Result<Value> {
        for attempt in 0..2 {
            let t = self.tokens().await;
            let mut req = self
                .http
                .request(method.clone(), format!("{API_BASE}{path}"))
                .query(query)
                .header("Authorization", format!("Bearer {}", t.access_token))
                .header("Client-Id", &t.client_id);
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            if resp.status().as_u16() == 401 && attempt == 0 {
                self.refresh().await?;
                continue;
            }
            let text = check(resp).await?.text().await?;
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return Ok(serde_json::from_str(&text)?);
        }
        unreachable!("the loop returns on its second attempt")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helix_errors_read_well() {
        let e = Error::Http { status: 400, body: r#"{"error":"Bad Request","status":400,"message":"The user is already banned."}"#.into() };
        assert_eq!(e.to_string(), "The user is already banned.");
    }
}
