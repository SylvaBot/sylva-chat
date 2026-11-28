//! The live chat session: Twitch IRC over a websocket, the history loaded on open, and the
//! batches of lines sent to the window.
//!
//! Busy chats: lines are built here and handed to the window at most every 50 ms as one batch,
//! so the window lays out and redraws once per batch however fast chat moves.

use crate::emotes::{self, Badge, Emote};
use crate::irc::{self, Message};
use crate::lines::{self, ChatEvent};
use crate::twitch::TwitchClient;
use crate::ui_events::{Notify, Ui};
use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_util::sync::CancellationToken;

const IRC_URL: &str = "wss://irc-ws.chat.twitch.tv:443";
const HISTORY: u32 = 150;
/// Twitch pings every few minutes; silence for longer means the socket died quietly.
const SILENCE: Duration = Duration::from_secs(6 * 60);
const BATCH_EVERY: Duration = Duration::from_millis(50);
/// More than this in one batch and only the newest are kept: the window shows 600 at most.
const BATCH_MAX: usize = 600;

/// What the rest of the app needs from a running chat.
pub struct Shared {
    pub channel_login: String,
    pub channel_id: String,
    pub me_id: String,
    pub me_login: String,
    /// Login to user id and display name, from chat, for moderation and the chatter card.
    pub users: Mutex<HashMap<String, (String, String)>>,
    pub emotes: RwLock<HashMap<String, Emote>>,
    pub badges: RwLock<HashMap<String, Badge>>,
    /// Shared Chat channels by id, to say where a message came from.
    pub rooms: RwLock<HashMap<String, String>>,
    /// EventSub delivers channel point redemptions, so chat leaves their messages to it.
    pub redemptions: std::sync::atomic::AtomicBool,
}

pub struct Session {
    pub shared: Arc<Shared>,
    pub cancel: CancellationToken,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Starts chat for the signed-in user's own channel.
pub fn start(notify: Notify, client: Arc<TwitchClient>, me_login: String, me_id: String) -> Session {
    let shared = Arc::new(Shared {
        channel_login: me_login.clone(),
        channel_id: me_id.clone(),
        me_id,
        me_login,
        users: Mutex::new(HashMap::new()),
        emotes: RwLock::new(HashMap::new()),
        badges: RwLock::new(HashMap::new()),
        rooms: RwLock::new(HashMap::new()),
        redemptions: std::sync::atomic::AtomicBool::new(false),
    });
    let cancel = CancellationToken::new();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(forward(notify.clone(), rx, cancel.clone()));
    tokio::spawn(crate::eventsub::run(notify.clone(), client.clone(), shared.clone(), tx.clone(), cancel.clone()));
    tokio::spawn(run(notify, client, shared.clone(), tx, cancel.clone()));
    Session { shared, cancel }
}

/// Sends what chat produced to the window in batches.
async fn forward(notify: Notify, mut rx: mpsc::UnboundedReceiver<ChatEvent>, cancel: CancellationToken) {
    let mut batch = Vec::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            got = rx.recv() => match got {
                Some(ev) => batch.push(ev),
                None => return,
            },
        }
        tokio::time::sleep(BATCH_EVERY).await;
        while let Ok(ev) = rx.try_recv() {
            batch.push(ev);
        }
        if batch.len() > BATCH_MAX {
            batch.drain(..batch.len() - BATCH_MAX);
        }
        notify(Ui::Chat(std::mem::take(&mut batch)));
    }
}

type Tx = mpsc::UnboundedSender<ChatEvent>;

fn sys(tx: &Tx, text: impl Into<String>) {
    let _ = tx.send(ChatEvent::Sys { text: text.into() });
}

async fn run(notify: Notify, client: Arc<TwitchClient>, shared: Arc<Shared>, tx: Tx, cancel: CancellationToken) {
    let http = match crate::twitch::http_client() {
        Ok(h) => h,
        Err(e) => return sys(&tx, format!("Could not start chat: {e}")),
    };
    let (emotes, badges) = tokio::join!(
        emotes::load_emotes(&http, &shared.channel_id),
        emotes::load_badges(&client, &shared.channel_id),
    );
    *shared.emotes.write().unwrap() = emotes;
    *shared.badges.write().unwrap() = badges;
    notify(Ui::ChatReady);

    load_history(&http, &shared, &tx).await;

    let mut backoff = Duration::from_secs(1);
    let mut auth_fails = 0u32;
    let mut was_joined = false;
    while !cancel.is_cancelled() {
        let outcome = tokio::select! {
            _ = cancel.cancelled() => return,
            o = connect_once(&client, &shared, &tx, auth_fails > 0, &mut was_joined) => o,
        };
        match outcome {
            Outcome::Joined => {
                backoff = Duration::from_secs(1);
                auth_fails = 0;
                sys(&tx, "Disconnected from chat. Reconnecting.");
            }
            Outcome::AuthFailed => {
                auth_fails += 1;
                if auth_fails > 2 {
                    sys(&tx, "Twitch did not accept the sign-in for chat. Sign out and sign in again.");
                    return;
                }
            }
            Outcome::Failed(e) => log::warn!("chat connection: {e}"),
            Outcome::Reconnect => backoff = Duration::from_millis(250),
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

enum Outcome {
    /// Was in chat, then the connection ended.
    Joined,
    AuthFailed,
    /// Twitch asked us to reconnect.
    Reconnect,
    Failed(String),
}

async fn connect_once(client: &TwitchClient, shared: &Shared, tx: &Tx, fresh: bool, was_joined: &mut bool) -> Outcome {
    if fresh {
        // Any Helix call refreshes an expired token.
        if let Err(e) = client.helix(Method::GET, "/users", &[], None).await {
            return Outcome::Failed(e.to_string());
        }
    }
    let token = client.tokens().await;
    let (mut ws, _) = match tokio_tungstenite::connect_async(IRC_URL).await {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let hello = [
        "CAP REQ :twitch.tv/tags twitch.tv/commands".to_string(),
        format!("PASS oauth:{}", token.access_token),
        format!("NICK {}", token.login),
        format!("JOIN #{}", shared.channel_login),
    ];
    for line in hello {
        if let Err(e) = ws.send(WsMessage::Text(line)).await {
            return Outcome::Failed(e.to_string());
        }
    }
    let mut joined = false;
    loop {
        let frame = match tokio::time::timeout(SILENCE, ws.next()).await {
            Err(_) => return if joined { Outcome::Joined } else { Outcome::Failed("no answer from Twitch".into()) },
            Ok(None) => return if joined { Outcome::Joined } else { Outcome::Failed("Twitch closed the connection".into()) },
            Ok(Some(Err(e))) => return if joined { Outcome::Joined } else { Outcome::Failed(e.to_string()) },
            Ok(Some(Ok(f))) => f,
        };
        let WsMessage::Text(text) = frame else { continue };
        for line in text.split("\r\n").filter(|l| !l.is_empty()) {
            let Some(m) = irc::parse(line) else { continue };
            match m.command.as_str() {
                "PING" => {
                    let _ = ws.send(WsMessage::Text(format!("PONG :{}", m.text()))).await;
                }
                "RECONNECT" => return Outcome::Reconnect,
                "NOTICE" if is_auth_failure(m.text()) => return Outcome::AuthFailed,
                "ROOMSTATE" if !joined => {
                    joined = true;
                    if *was_joined {
                        sys(tx, "Reconnected.");
                    }
                    *was_joined = true;
                }
                _ => handle(&m, false, shared, tx),
            }
        }
    }
}

fn is_auth_failure(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("authentication failed") || t.contains("improperly formatted auth")
}

async fn load_history(http: &reqwest::Client, shared: &Shared, tx: &Tx) {
    let url = format!("https://recent-messages.robotty.de/api/v2/recent-messages/{}?limit={HISTORY}", shared.channel_login);
    let body: Value = match http.get(&url).send().await {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
        _ => return,
    };
    let lines: Vec<&str> = body["messages"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    for line in &lines {
        if let Some(m) = irc::parse(line) {
            if matches!(m.command.as_str(), "PRIVMSG" | "USERNOTICE" | "CLEARCHAT" | "CLEARMSG") {
                handle(&m, true, shared, tx);
            }
        }
    }
    if !lines.is_empty() {
        let _ = tx.send(ChatEvent::Divider { text: "Live".into() });
    }
}

fn remember(shared: &Shared, m: &Message, login: &str) {
    let id = m.tag("user-id");
    if login.is_empty() || id.is_empty() {
        return;
    }
    let name = if m.tag("display-name").is_empty() { login } else { m.tag("display-name") };
    let mut users = shared.users.lock().unwrap();
    if users.len() > 20_000 {
        users.clear();
    }
    users.insert(login.to_string(), (id.to_string(), name.to_string()));
}

fn handle(m: &Message, old: bool, shared: &Shared, tx: &Tx) {
    let ev = match m.command.as_str() {
        // A live redemption with text arrives from EventSub too, as a card with the reward's name.
        "PRIVMSG" if !old && !m.tag("custom-reward-id").is_empty() && shared.redemptions.load(std::sync::atomic::Ordering::Relaxed) => {
            remember(shared, m, m.nick());
            return;
        }
        "PRIVMSG" => {
            remember(shared, m, m.nick());
            let mut ev = lines::message(m, old, &shared.me_login, &shared.emotes.read().unwrap(), &shared.badges.read().unwrap());
            // Shared Chat: name the channel a message came from instead of its id.
            if let ChatEvent::Msg { source: Some(src), .. } = &mut ev {
                *src = shared.rooms.read().unwrap().get(src.as_str()).cloned().unwrap_or_else(|| "Shared Chat".into());
            }
            ev
        }
        "USERNOTICE" => {
            remember(shared, m, m.tag("login"));
            lines::event(m, old, &shared.emotes.read().unwrap(), &shared.badges.read().unwrap())
        }
        "CLEARMSG" => ChatEvent::Deleted { id: m.tag("target-msg-id").to_string() },
        "CLEARCHAT" => match m.params.get(1) {
            None => {
                let _ = tx.send(ChatEvent::Cleared);
                ChatEvent::Sys { text: "A moderator cleared chat.".into() }
            }
            Some(login) => {
                let _ = tx.send(ChatEvent::UserCleared { login: login.clone() });
                let who = shared.users.lock().unwrap().get(login).map(|u| u.1.clone()).unwrap_or_else(|| login.clone());
                let text = match m.tag("ban-duration").parse::<u64>() {
                    Ok(s) => format!("{who} was timed out for {}.", lines::duration(s)),
                    Err(_) => format!("{who} was banned."),
                };
                ChatEvent::Sys { text }
            }
        },
        "NOTICE" if !old => ChatEvent::Sys { text: m.text().to_string() },
        _ => return,
    };
    let _ = tx.send(ev);
}
