//! Sign-in, chat start and stop, sending and moderation. The window calls these on the tokio
//! runtime and gets results back as [`Ui`] events.

use crate::chat::{self, Session as ChatSession};
use crate::lines::duration;
use crate::secrets;
use crate::twitch::{self, Error, TwitchClient, TwitchTokens};
use crate::ui_events::{Notify, Ui, User};
use reqwest::Method;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

type CmdResult<T> = Result<T, String>;

/// Reading chat, sending as yourself, deleting messages, timeouts and bans.
const SCOPES: &str = "chat:read user:write:chat moderator:manage:banned_users moderator:manage:chat_messages channel:read:hype_train channel:read:polls channel:read:predictions channel:manage:vips channel:manage:moderators moderator:manage:shoutouts channel:read:redemptions";

pub struct Backend {
    notify: Notify,
    twitch: RwLock<Option<Arc<TwitchClient>>>,
    login: Mutex<Option<CancellationToken>>,
    chat: Mutex<Option<ChatSession>>,
}

fn twitch_err(e: Error) -> String {
    if e.to_string().to_lowercase().contains("missing scope") {
        "Sylva Chat needs one more permission for this. Sign out and sign in again to allow it.".into()
    } else if e.is_unauthorized() {
        format!("Twitch said no ({e}). If this keeps happening, sign out and sign in again.")
    } else {
        e.to_string()
    }
}

fn client_for(tokens: TwitchTokens) -> CmdResult<Arc<TwitchClient>> {
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

impl Backend {
    pub fn new(notify: Notify) -> Arc<Self> {
        Arc::new(Self { notify, twitch: RwLock::new(None), login: Mutex::new(None), chat: Mutex::new(None) })
    }

    pub fn toast(&self, text: impl Into<String>, bad: bool) {
        (self.notify)(Ui::Toast { text: text.into(), bad });
    }

    async fn client(&self) -> CmdResult<Arc<TwitchClient>> {
        self.twitch.read().await.clone().ok_or_else(|| "Sign in to Twitch first.".to_string())
    }

    async fn running(&self) -> CmdResult<Arc<chat::Shared>> {
        self.chat.lock().await.as_ref().map(|s| s.shared.clone()).ok_or_else(|| "Chat is not running.".to_string())
    }

    /// On launch: use the saved sign-in, or show the sign-in screen.
    pub async fn restore(&self) {
        if std::env::var_os("SYLVA_CHAT_DEMO").is_some() {
            return self.demo();
        }
        match secrets::load_twitch() {
            Ok(Some(tokens)) => match client_for(tokens) {
                Ok(c) => {
                    *self.twitch.write().await = Some(c);
                    self.start_chat().await;
                    return;
                }
                Err(e) => self.toast(e, true),
            },
            Ok(None) => {}
            Err(e) => self.toast(e, true),
        }
        (self.notify)(Ui::SignedOut { suggested_client_id: secrets::desktop_client_id() });
    }

    /// Sample chat for checking the look without signing in (SYLVA_CHAT_DEMO=1).
    fn demo(&self) {
        use crate::{irc, lines};
        use std::collections::HashMap;
        (self.notify)(Ui::SignedIn(User { login: "haruxee".into(), user_id: "1".into() }));
        (self.notify)(Ui::ChatReady);
        let samples = [
            "@badges=broadcaster/1;color=#0000FF;display-name=Haruxee;emotes=25:6-10;id=a1;tmi-sent-ts=1700000000000 :haruxee!h@h PRIVMSG #haruxee :hello Kappa welcome in",
            "@badges=;color=;display-name=viewer1;first-msg=1;id=a2;reply-parent-display-name=Haruxee;reply-parent-user-login=haruxee;reply-parent-msg-body=hello\\sKappa :viewer1!v@v PRIVMSG #haruxee :@haruxee nice stream, check twitch.tv/haruxee",
            "@badges=;color=#123456;display-name=Lurker;id=a3 :lurker!l@l PRIVMSG #haruxee :\u{1}ACTION waves 👋🏽 love this ❤️\u{1}",
            "@badges=;color=#FF7F50;display-name=LongTalker;id=a4 :longtalker!l@l PRIVMSG #haruxee :this is a much longer message that should wrap onto a second line so we can see how wrapping looks in a narrow window",
            "@badges=;color=#9ACD32;display-name=FriendViewer;id=a6;room-id=1;source-room-id=2 :friendviewer!f@f PRIVMSG #haruxee :hi from the other side",
            "@display-name=Haruxee;login=haruxee;msg-id=announcement;msg-param-color=BLUE;id=a7;color=#0000FF :tmi.twitch.tv USERNOTICE #haruxee :Giveaway at 100 followers",
            "@display-name=Gifter;login=gifter;system-msg=Gifter\\ssubscribed\\sfor\\s3\\smonths!;id=a5;color=#FF0000 :tmi.twitch.tv USERNOTICE #haruxee :love it here",
        ];
        let mut batch = Vec::new();
        for line in samples {
            let line = line.replace("\\u{1}", "\u{1}");
            if let Some(m) = irc::parse(&line) {
                batch.push(match m.command.as_str() {
                    "USERNOTICE" => lines::event(&m, false, &HashMap::new(), &HashMap::new()),
                    _ => lines::message(&m, false, "haruxee", &HashMap::new(), &HashMap::new()),
                });
            }
        }
        batch.push(lines::ChatEvent::Sys { text: "Lurker was timed out for 10 minutes.".into() });
        for ev in &mut batch {
            if let lines::ChatEvent::Msg { source: Some(s), .. } = ev {
                *s = "FriendStreamer".into();
            }
        }
        (self.notify)(Ui::Chat(batch));
        use crate::live::*;
        use serde_json::json;
        let soon = |s: i64| (chrono::Utc::now() + chrono::Duration::seconds(s)).to_rfc3339();
        let demo = [
            ("channel.shared_chat.begin", json!({"participants": [
                {"broadcaster_user_id": "1", "broadcaster_user_name": "Haruxee"},
                {"broadcaster_user_id": "2", "broadcaster_user_name": "FriendStreamer"}]})),
            ("channel.hype_train.progress", json!({"id": "t", "type": "golden_kappa", "level": 3, "progress": 1200, "goal": 1800,
                "expires_at": soon(214), "all_time_high_level": 2,
                "top_contributions": [{"user_name": "Gifter", "type": "subscription", "total": 2500}, {"user_name": "BitsFan", "type": "bits", "total": 800}],
                "shared_train_participants": [{"broadcaster_user_id": "1", "broadcaster_user_name": "Haruxee"}, {"broadcaster_user_id": "2", "broadcaster_user_name": "FriendStreamer"}]})),
            ("channel.prediction.progress", json!({"id": "pr", "title": "Do we beat the boss first try?", "locks_at": soon(95),
                "outcomes": [{"id": "a", "title": "Yes", "color": "blue", "users": 41, "channel_points": 52000},
                             {"id": "b", "title": "No way", "color": "pink", "users": 63, "channel_points": 81000}]})),
            ("channel.poll.progress", json!({"id": "po", "title": "Next game?", "ends_at": soon(42),
                "choices": [{"title": "Hollow Knight", "votes": 18}, {"title": "Celeste", "votes": 27}, {"title": "Outer Wilds", "votes": 9}]})),
        ];
        for (kind, e) in demo {
            if let Some(l) = from_notification(kind, &e, "1") {
                (self.notify)(Ui::Live(l));
            }
        }
    }

    async fn start_chat(&self) {
        let Ok(c) = self.client().await else { return };
        let t = c.tokens().await;
        let session = chat::start(self.notify.clone(), c, t.login.clone(), t.user_id.clone());
        *self.chat.lock().await = Some(session);
        (self.notify)(Ui::SignedIn(User { login: t.login, user_id: t.user_id }));
    }

    /// Twitch's device sign-in: shows a code, then waits for the user to approve it.
    pub async fn sign_in(self: Arc<Self>, client_id: String) {
        let client_id = client_id.trim().to_string();
        if client_id.is_empty() {
            return (self.notify)(Ui::SignInFailed("Paste your Twitch Client ID first.".into()));
        }
        let result = async {
            let http = twitch::http_client().map_err(|e| e.to_string())?;
            let code = twitch::start_device_flow(&http, &client_id, SCOPES).await.map_err(|e| e.to_string())?;
            Ok::<_, String>((http, code))
        }
        .await;
        let (http, code) = match result {
            Ok(v) => v,
            Err(e) => return (self.notify)(Ui::SignInFailed(e)),
        };
        let cancel = CancellationToken::new();
        if let Some(old) = self.login.lock().await.replace(cancel.clone()) {
            old.cancel();
        }
        let _ = open::that_detached(&code.verification_uri);
        (self.notify)(Ui::DeviceCode(code.clone()));
        match twitch::finish_device_flow(&http, &client_id, SCOPES, &code, &cancel).await {
            Ok(tokens) => {
                if let Err(e) = secrets::save_twitch(&tokens) {
                    log::warn!("{e}");
                }
                match client_for(tokens) {
                    Ok(c) => {
                        *self.twitch.write().await = Some(c);
                        self.start_chat().await;
                    }
                    Err(e) => (self.notify)(Ui::SignInFailed(e)),
                }
            }
            Err(e) if !cancel.is_cancelled() => (self.notify)(Ui::SignInFailed(e.to_string())),
            Err(_) => {}
        }
    }

    pub async fn cancel_sign_in(&self) {
        if let Some(c) = self.login.lock().await.take() {
            c.cancel();
        }
    }

    pub async fn sign_out(&self) {
        *self.chat.lock().await = None;
        *self.twitch.write().await = None;
        if let Err(e) = secrets::clear_twitch() {
            self.toast(e, true);
        }
        (self.notify)(Ui::SignedOut { suggested_client_id: secrets::desktop_client_id() });
    }

    /// Emote codes for Tab completion, shortest first.
    pub async fn emote_codes(&self) -> Vec<String> {
        let Ok(shared) = self.running().await else { return Vec::new() };
        let mut codes: Vec<String> = shared.emotes.read().unwrap().keys().cloned().collect();
        codes.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        codes
    }
}

async fn user_id(client: &TwitchClient, shared: &chat::Shared, login: &str) -> CmdResult<String> {
    let login = login.trim_start_matches('@').to_lowercase();
    if let Some((id, _)) = shared.users.lock().unwrap().get(&login) {
        return Ok(id.clone());
    }
    let r = client.helix(Method::GET, "/users", &[("login", login.clone())], None).await.map_err(twitch_err)?;
    r["data"][0]["id"].as_str().map(str::to_string).ok_or_else(|| format!("No Twitch user called {login}."))
}

fn mod_query(shared: &chat::Shared) -> Vec<(&'static str, String)> {
    vec![("broadcaster_id", shared.channel_id.clone()), ("moderator_id", shared.me_id.clone())]
}

/// Time out (`seconds` > 0) or ban (`seconds` 0).
async fn ban(client: &TwitchClient, shared: &chat::Shared, login: &str, seconds: u64, reason: &str) -> CmdResult<()> {
    let mut data = serde_json::json!({ "user_id": user_id(client, shared, login).await? });
    if seconds > 0 {
        data["duration"] = seconds.into();
    }
    if !reason.is_empty() {
        data["reason"] = reason.into();
    }
    client
        .helix(Method::POST, "/moderation/bans", &mod_query(shared), Some(&serde_json::json!({ "data": data })))
        .await
        .map_err(twitch_err)?;
    Ok(())
}

async fn unban(client: &TwitchClient, shared: &chat::Shared, login: &str) -> CmdResult<()> {
    let mut q = mod_query(shared);
    q.push(("user_id", user_id(client, shared, login).await?));
    client.helix(Method::DELETE, "/moderation/bans", &q, None).await.map_err(twitch_err)?;
    Ok(())
}

impl Backend {
/// Delete a message, time out, ban or unban. Returns text to show when it worked.
pub async fn moderate(
    &self,
    action: &str,
    login: String,
    message_id: Option<String>,
    seconds: Option<u64>,
) -> CmdResult<Option<String>> {
    let client = self.client().await?;
    let shared = self.running().await?;
    let name = shared.users.lock().unwrap().get(&login).map(|u| u.1.clone()).unwrap_or_else(|| login.clone());
    match action {
        "delete" => {
            let mut q = mod_query(&shared);
            q.push(("message_id", message_id.ok_or("Which message?")?));
            client.helix(Method::DELETE, "/moderation/chat", &q, None).await.map_err(twitch_err)?;
            Ok(None)
        }
        "timeout" => {
            let s = seconds.unwrap_or(600).max(1);
            ban(&client, &shared, &login, s, "").await?;
            Ok(Some(format!("{name} timed out for {}.", duration(s))))
        }
        "ban" => {
            ban(&client, &shared, &login, 0, "").await?;
            Ok(Some(format!("{name} is banned.")))
        }
        "unban" => {
            unban(&client, &shared, &login).await?;
            Ok(Some(format!("{name} can chat again.")))
        }
        "vip" | "unvip" | "mod" | "unmod" => {
            let q = vec![("broadcaster_id", shared.channel_id.clone()), ("user_id", user_id(&client, &shared, &login).await?)];
            let (path, method) = match action {
                "vip" => ("/channels/vips", Method::POST),
                "unvip" => ("/channels/vips", Method::DELETE),
                "mod" => ("/moderation/moderators", Method::POST),
                _ => ("/moderation/moderators", Method::DELETE),
            };
            client.helix(method, path, &q, None).await.map_err(twitch_err)?;
            Ok(Some(match action {
                "vip" => format!("{name} is a VIP now."),
                "unvip" => format!("{name} is no longer a VIP."),
                "mod" => format!("{name} is a moderator now."),
                _ => format!("{name} is no longer a moderator."),
            }))
        }
        "shoutout" => {
            let q = vec![
                ("from_broadcaster_id", shared.channel_id.clone()),
                ("to_broadcaster_id", user_id(&client, &shared, &login).await?),
                ("moderator_id", shared.me_id.clone()),
            ];
            client.helix(Method::POST, "/chat/shoutouts", &q, None).await.map_err(twitch_err)?;
            Ok(Some(format!("Shoutout sent for {name}.")))
        }
        _ => Err(format!("unknown action {action}")),
    }
}
}

const HELP: &str = "Commands: /ban name [reason], /unban name, /timeout name [10m] [reason], /untimeout name, /clear. Start with // to send a message that begins with /.";

/// "90", "90s", "10m", "1h", "1d" in seconds.
fn parse_duration(s: &str) -> Option<u64> {
    let (n, unit) = match s.char_indices().last()? {
        (i, c) if c.is_ascii_alphabetic() => (&s[..i], c.to_ascii_lowercase()),
        _ => (s, 's'),
    };
    let n: u64 = n.parse().ok()?;
    Some(n * match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86400,
        _ => return None,
    })
}

/// Sends a message as the streamer, or runs a /command. Returns text to show, if any.
impl Backend {
pub async fn send_chat(&self, text: String, reply_to: Option<String>) -> CmdResult<Option<String>> {
    let client = self.client().await?;
    let shared = self.running().await?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if let Some(cmd) = text.strip_prefix('/').filter(|c| !c.starts_with('/')) {
        let mut words = cmd.split_whitespace();
        let name = words.next().unwrap_or("").to_lowercase();
        let who = words.next();
        let need = || who.map(|w| w.trim_start_matches('@').to_lowercase()).ok_or_else(|| format!("Add a name: /{name} name"));
        let rest: Vec<&str> = words.collect();
        return match name.as_str() {
            "ban" => {
                ban(&client, &shared, &need()?, 0, &rest.join(" ")).await?;
                Ok(Some(format!("{} is banned.", who.unwrap_or_default())))
            }
            "unban" | "untimeout" => {
                unban(&client, &shared, &need()?).await?;
                Ok(Some(format!("{} can chat again.", who.unwrap_or_default())))
            }
            "timeout" => {
                let login = need()?;
                let (secs, reason) = match rest.first().and_then(|r| parse_duration(r)) {
                    Some(s) => (s, rest[1..].join(" ")),
                    None => (600, rest.join(" ")),
                };
                ban(&client, &shared, &login, secs, &reason).await?;
                Ok(Some(format!("{} timed out for {}.", who.unwrap_or_default(), duration(secs))))
            }
            "clear" => {
                client.helix(Method::DELETE, "/moderation/chat", &mod_query(&shared), None).await.map_err(twitch_err)?;
                Ok(None)
            }
            "help" => Ok(Some(HELP.into())),
            _ => Err(format!("Sylva Chat does not know /{name}. {HELP}")),
        };
    }
    let message = text.strip_prefix('/').unwrap_or(text);
    let mut body = serde_json::json!({ "broadcaster_id": shared.channel_id, "sender_id": shared.me_id, "message": message });
    if let Some(id) = reply_to.filter(|r| !r.is_empty()) {
        body["reply_parent_message_id"] = id.into();
    }
    let r = client.helix(Method::POST, "/chat/messages", &[], Some(&body)).await.map_err(twitch_err)?;
    let sent = &r["data"][0];
    if sent["is_sent"] == false {
        return Err(sent["drop_reason"]["message"].as_str().unwrap_or("Twitch did not send that message.").to_string());
    }
    Ok(None)
}
}

/// Signs the update files; the private half is the SylvaBot secret TAURI_SIGNING_PRIVATE_KEY.
const UPDATE_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDgwMjM3OURFNEE0MUM0MEMKUldRTXhFRkszbmtqZ09MZUdHS3V6VlRHd1Ivamhod0FpVUFUdVdya2FjNlFrUjRncFhtbWpLODUK";
const UPDATE_URL: &str = "https://github.com/SylvaBot/sylva-chat/releases/latest/download/latest.json";

fn updater_config() -> cargo_packager_updater::Config {
    cargo_packager_updater::Config {
        endpoints: vec![UPDATE_URL.parse().expect("valid update URL")],
        pubkey: UPDATE_PUBKEY.into(),
        windows: Some(cargo_packager_updater::WindowsConfig {
            install_mode: Some(cargo_packager_updater::WindowsUpdateInstallMode::Passive),
            ..Default::default()
        }),
    }
}

fn current_version() -> semver::Version {
    env!("CARGO_PKG_VERSION").parse().expect("valid version")
}

impl Backend {
    /// Tells the window when a newer version is out.
    pub async fn check_for_update(&self) {
        let found = tokio::task::spawn_blocking(|| cargo_packager_updater::check_update(current_version(), updater_config())).await;
        match found {
            Ok(Ok(Some(u))) => (self.notify)(Ui::UpdateReady(u.version)),
            Ok(Ok(None)) => {}
            Ok(Err(e)) => log::warn!("update check: {e}"),
            Err(e) => log::warn!("update check: {e}"),
        }
    }

    /// Downloads and installs the newest version. On Windows the installer closes Sylva Chat
    /// and opens the new one.
    pub async fn install_update(&self) {
        self.toast("Downloading the update. Sylva Chat restarts by itself.", false);
        let result = tokio::task::spawn_blocking(|| -> Result<(), String> {
            let update = cargo_packager_updater::check_update(current_version(), updater_config())
                .map_err(|e| e.to_string())?
                .ok_or("Sylva Chat is already up to date.")?;
            update.download_and_install().map_err(|e| e.to_string())
        })
        .await;
        match result {
            Ok(Ok(())) => {
                // macOS and Linux replace the app in place; start the new one.
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).spawn();
                }
                std::process::exit(0);
            }
            Ok(Err(e)) => self.toast(format!("The update did not install: {e}"), true),
            Err(e) => self.toast(format!("The update did not install: {e}"), true),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations() {
        assert_eq!(super::parse_duration("90"), Some(90));
        assert_eq!(super::parse_duration("10m"), Some(600));
        assert_eq!(super::parse_duration("1D"), Some(86400));
        assert_eq!(super::parse_duration("soon"), None);
    }
}

