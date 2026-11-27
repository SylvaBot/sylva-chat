//! Commands the window calls with `invoke`. Chat itself (Twitch IRC over a websocket) runs in
//! the window; Rust keeps the token, makes Helix calls and fetches emote lists.

use crate::secrets;
use crate::twitch::{self, DeviceCode, Error, TwitchClient, TwitchTokens};
use reqwest::Method;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

type CmdResult<T> = Result<T, String>;

/// Reading chat, sending as yourself, deleting messages, timeouts and bans.
const SCOPES: &str = "chat:read user:write:chat moderator:manage:banned_users moderator:manage:chat_messages";

/// Emote, badge and history services the window may read through [`fetch_json`].
const FETCH_HOSTS: &[&str] = &["7tv.io", "api.betterttv.net", "api.frankerfacez.com", "recent-messages.robotty.de"];

#[derive(Default)]
pub struct AppState {
    twitch: RwLock<Option<Arc<TwitchClient>>>,
    login: Mutex<Option<CancellationToken>>,
    http: std::sync::OnceLock<reqwest::Client>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    user: Option<User>,
    /// Client ID to suggest on the sign-in screen (the one Sylva Control uses).
    suggested_client_id: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct User {
    login: String,
    user_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatToken {
    access_token: String,
    client_id: String,
    login: String,
    user_id: String,
}

fn twitch_err(e: Error) -> String {
    if e.is_unauthorized() {
        format!("Twitch said no ({e}). If this keeps happening, sign out and sign in again.")
    } else {
        e.to_string()
    }
}

fn client(tokens: TwitchTokens) -> CmdResult<Arc<TwitchClient>> {
    TwitchClient::new(
        tokens,
        Some(Box::new(|t: &TwitchTokens| {
            if let Err(e) = secrets::save_twitch(t) {
                log::warn!("{e}");
            }
        })),
    )
    .map_err(|e| e.to_string())
}

async fn current(state: &AppState) -> CmdResult<Arc<TwitchClient>> {
    state.twitch.read().await.clone().ok_or_else(|| "Sign in to Twitch first.".to_string())
}

async fn session(state: &AppState) -> Session {
    let user = match state.twitch.read().await.as_ref() {
        Some(c) => {
            let t = c.tokens().await;
            Some(User { login: t.login, user_id: t.user_id })
        }
        None => None,
    };
    Session { user, suggested_client_id: secrets::desktop_client_id() }
}

#[tauri::command]
pub async fn restore_session(state: State<'_, AppState>) -> CmdResult<Session> {
    if state.twitch.read().await.is_none() {
        if let Some(tokens) = secrets::load_twitch()? {
            *state.twitch.write().await = Some(client(tokens)?);
        }
    }
    Ok(session(&state).await)
}

/// Starts Twitch's device sign-in. The window shows the code; `signed-in` or `sign-in-failed`
/// follows.
#[tauri::command]
pub async fn sign_in(app: AppHandle, state: State<'_, AppState>, client_id: String) -> CmdResult<DeviceCode> {
    let client_id = client_id.trim().to_string();
    if client_id.is_empty() {
        return Err("Paste your Twitch Client ID first.".into());
    }
    let http = twitch::http_client().map_err(|e| e.to_string())?;
    let code = twitch::start_device_flow(&http, &client_id, SCOPES)
        .await
        .map_err(|e| e.to_string())?;
    let cancel = CancellationToken::new();
    if let Some(old) = state.login.lock().await.replace(cancel.clone()) {
        old.cancel();
    }
    let _ = app.opener().open_url(&code.verification_uri, None::<&str>);
    let pending = code.clone();
    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        let result = twitch::finish_device_flow(&http, &client_id, SCOPES, &pending, &cancel).await;
        let state = app.state::<AppState>();
        match result {
            Ok(tokens) => {
                if let Err(e) = secrets::save_twitch(&tokens) {
                    log::warn!("{e}");
                }
                let user = User { login: tokens.login.clone(), user_id: tokens.user_id.clone() };
                match client(tokens) {
                    Ok(c) => {
                        *state.twitch.write().await = Some(c);
                        let _ = app.emit("signed-in", user);
                    }
                    Err(e) => {
                        let _ = app.emit("sign-in-failed", e);
                    }
                }
            }
            Err(e) if !cancel.is_cancelled() => {
                let _ = app.emit("sign-in-failed", e.to_string());
            }
            Err(_) => {}
        }
    });
    Ok(code)
}

#[tauri::command]
pub async fn cancel_sign_in(state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(c) = state.login.lock().await.take() {
        c.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn sign_out(state: State<'_, AppState>) -> CmdResult<Session> {
    *state.twitch.write().await = None;
    secrets::clear_twitch()?;
    Ok(session(&state).await)
}

/// The token for Twitch IRC. With `fresh`, a Helix call first makes sure it still works
/// (refreshing it if Twitch says it expired).
#[tauri::command]
pub async fn chat_token(state: State<'_, AppState>, fresh: Option<bool>) -> CmdResult<ChatToken> {
    let c = current(&state).await?;
    if fresh.unwrap_or(false) {
        c.helix(Method::GET, "/users", &[], None).await.map_err(twitch_err)?;
    }
    let t = c.tokens().await;
    Ok(ChatToken { access_token: t.access_token, client_id: t.client_id, login: t.login, user_id: t.user_id })
}

/// Any Helix call, signed in as the user (badges, sending, deleting, timeouts, bans).
#[tauri::command]
pub async fn helix(
    state: State<'_, AppState>,
    method: String,
    path: String,
    query: Option<Vec<(String, String)>>,
    body: Option<Value>,
) -> CmdResult<Value> {
    let method = Method::from_bytes(method.to_ascii_uppercase().as_bytes()).map_err(|e| e.to_string())?;
    if !path.starts_with('/') || path.contains("..") {
        return Err("bad Helix path".into());
    }
    let query: Vec<(&str, String)> = query.iter().flatten().map(|(k, v)| (k.as_str(), v.clone())).collect();
    current(&state).await?.helix(method, &path, &query, body.as_ref()).await.map_err(twitch_err)
}

/// GET a JSON document from one of [`FETCH_HOSTS`] (emote sets, chat history).
#[tauri::command]
pub async fn fetch_json(state: State<'_, AppState>, url: String) -> CmdResult<Value> {
    let parsed = url::Url::parse(&url).map_err(|e| e.to_string())?;
    let host = parsed.host_str().unwrap_or_default().to_string();
    if parsed.scheme() != "https" || !FETCH_HOSTS.contains(&host.as_str()) {
        return Err(format!("{host} is not an emote or history service Sylva Chat knows"));
    }
    let http = match state.http.get() {
        Some(h) => h.clone(),
        None => {
            let h = twitch::http_client().map_err(|e| e.to_string())?;
            state.http.get_or_init(|| h).clone()
        }
    };
    let resp = http.get(parsed).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.as_u16() == 404 {
        return Ok(Value::Null);
    }
    if !status.is_success() {
        return Err(format!("{host} answered HTTP {}", status.as_u16()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_external(app: AppHandle, url: String) -> CmdResult<()> {
    if !url.starts_with("https://") {
        return Err("only https links open".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    version: Option<String>,
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> CmdResult<UpdateInfo> {
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    Ok(UpdateInfo { version: update.map(|u| u.version) })
}

/// Downloads and installs the newest release, then restarts.
#[tauri::command]
pub async fn apply_update(app: AppHandle) -> CmdResult<()> {
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Sylva Chat is already up to date.")?;
    update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    app.restart();
}
