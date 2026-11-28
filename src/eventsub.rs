//! Live channel events over Twitch's EventSub websocket: hype trains, polls, predictions and
//! Shared Chat. On open, whatever is already running is read from Helix so it shows at once.

use crate::chat::Shared;
use crate::lines::ChatEvent;
use crate::live::{self, Live, Participant};
use crate::twitch::{Error, TwitchClient};
use crate::ui_events::{Notify, Ui};
use futures_util::StreamExt;
use reqwest::Method;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_util::sync::CancellationToken;

const WS_URL: &str = "wss://eventsub.wss.twitch.tv/ws?keepalive_timeout_seconds=30";

const SUBSCRIPTIONS: &[(&str, &str)] = &[
    ("channel.hype_train.begin", "2"),
    ("channel.hype_train.progress", "2"),
    ("channel.hype_train.end", "2"),
    ("channel.poll.begin", "1"),
    ("channel.poll.progress", "1"),
    ("channel.poll.end", "1"),
    ("channel.prediction.begin", "1"),
    ("channel.prediction.progress", "1"),
    ("channel.prediction.lock", "1"),
    ("channel.prediction.end", "1"),
    ("channel.shared_chat.begin", "1"),
    ("channel.shared_chat.update", "1"),
    ("channel.shared_chat.end", "1"),
];

type Tx = mpsc::UnboundedSender<ChatEvent>;

enum Next {
    /// Twitch asked us to move to a new URL; subscriptions move with us.
    MoveTo(String),
    /// The connection dropped; start over with fresh subscriptions.
    Retry,
}

pub async fn run(notify: Notify, client: Arc<TwitchClient>, shared: Arc<Shared>, tx: Tx, cancel: CancellationToken) {
    tokio::select! {
        _ = cancel.cancelled() => return,
        _ = current(&notify, &client, &shared) => {}
    }
    let mut url = WS_URL.to_string();
    let mut subscribed = false;
    let mut told_scopes = false;
    let mut backoff = Duration::from_secs(1);
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => return,
            n = session(&url, subscribed, &notify, &client, &shared, &tx, &mut told_scopes) => n,
        };
        match next {
            Next::MoveTo(u) => {
                url = u;
                subscribed = true;
                continue;
            }
            Next::Retry => {
                url = WS_URL.to_string();
                subscribed = false;
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// One websocket connection, until it drops or Twitch moves us.
async fn session(
    url: &str,
    mut subscribed: bool,
    notify: &Notify,
    client: &TwitchClient,
    shared: &Shared,
    tx: &Tx,
    told_scopes: &mut bool,
) -> Next {
    let mut ws = match tokio_tungstenite::connect_async(url).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            log::warn!("eventsub connect: {e}");
            return Next::Retry;
        }
    };
    let mut keepalive = Duration::from_secs(40);
    loop {
        let frame = match tokio::time::timeout(keepalive, ws.next()).await {
            Ok(Some(Ok(f))) => f,
            _ => return Next::Retry,
        };
        let WsMessage::Text(text) = frame else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let payload = &v["payload"];
        match v["metadata"]["message_type"].as_str().unwrap_or_default() {
            "session_welcome" => {
                let session = &payload["session"];
                keepalive = Duration::from_secs(session["keepalive_timeout_seconds"].as_u64().unwrap_or(30) + 10);
                if !subscribed {
                    let id = session["id"].as_str().unwrap_or_default();
                    if !subscribe(client, shared, id, tx, told_scopes).await {
                        return Next::Retry;
                    }
                    subscribed = true;
                }
            }
            "session_reconnect" => {
                if let Some(u) = payload["session"]["reconnect_url"].as_str() {
                    return Next::MoveTo(u.to_string());
                }
            }
            "notification" => {
                let kind = payload["subscription"]["type"].as_str().unwrap_or_default();
                if let Some(l) = live::from_notification(kind, &payload["event"], &shared.channel_id) {
                    publish(notify, shared, l);
                }
            }
            _ => {}
        }
    }
}

/// Subscribes this session to everything Sylva Chat shows. False when the connection is no use.
async fn subscribe(client: &TwitchClient, shared: &Shared, session_id: &str, tx: &Tx, told_scopes: &mut bool) -> bool {
    let mut missing = false;
    for (kind, version) in SUBSCRIPTIONS {
        let body = json!({
            "type": kind,
            "version": version,
            "condition": { "broadcaster_user_id": shared.channel_id },
            "transport": { "method": "websocket", "session_id": session_id },
        });
        match client.helix(Method::POST, "/eventsub/subscriptions", &[], Some(&body)).await {
            Ok(_) => {}
            // Signed in before Sylva Chat asked for these; the rest still work.
            Err(Error::Http { status: 403, .. }) => missing = true,
            Err(e) if e.is_unauthorized() => return false,
            Err(e) => log::warn!("eventsub {kind}: {e}"),
        }
    }
    if missing && !*told_scopes {
        *told_scopes = true;
        let _ = tx.send(ChatEvent::Sys {
            text: "To see hype trains, polls and predictions here, sign out and sign in again so Twitch can allow them.".into(),
        });
    }
    true
}

/// Shows what is already running when chat opens.
async fn current(notify: &Notify, client: &TwitchClient, shared: &Shared) {
    let q = [("broadcaster_id", shared.channel_id.clone())];
    let first = [("broadcaster_id", shared.channel_id.clone()), ("first", "1".to_string())];
    let (train, poll, pred, chat) = tokio::join!(
        client.helix(Method::GET, "/hypetrain/status", &q, None),
        client.helix(Method::GET, "/polls", &first, None),
        client.helix(Method::GET, "/predictions", &first, None),
        client.helix(Method::GET, "/shared_chat/session", &q, None),
    );
    let found = [
        train.ok().and_then(|v| live::train_from_helix(&v, &shared.channel_id)),
        poll.ok().and_then(|v| live::poll_from_helix(&v)),
        pred.ok().and_then(|v| live::prediction_from_helix(&v)),
    ];
    for l in found.into_iter().flatten() {
        notify(Ui::Live(l));
    }
    // Helix lists Shared Chat channels by id only; look up their names.
    let ids: Vec<String> = chat
        .ok()
        .and_then(|v| v["data"][0]["participants"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p["broadcaster_id"].as_str().map(str::to_string))
        .collect();
    if ids.len() > 1 {
        let query: Vec<(&str, String)> = ids.iter().map(|id| ("id", id.clone())).collect();
        if let Ok(v) = client.helix(Method::GET, "/users", &query, None).await {
            let people = v["data"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|u| Participant {
                            id: u["id"].as_str().unwrap_or_default().into(),
                            login: u["login"].as_str().unwrap_or_default().into(),
                            name: u["display_name"].as_str().unwrap_or_default().into(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            publish(notify, shared, Live::Shared(people));
        }
    }
}

fn publish(notify: &Notify, shared: &Shared, l: Live) {
    if let Live::Shared(people) = &l {
        let mut rooms = shared.rooms.write().unwrap();
        rooms.clear();
        for p in people {
            rooms.insert(p.id.clone(), p.name.clone());
        }
    }
    notify(Ui::Live(l));
}
