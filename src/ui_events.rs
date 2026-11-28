//! What background work tells the window. Each send also asks the window to redraw.

use crate::lines::ChatEvent;
use crate::twitch::DeviceCode;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct User {
    pub login: String,
    pub user_id: String,
}

/// What Twitch says about a chatter, for their card.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub login: String,
    pub id: String,
    pub name: String,
    pub avatar: String,
    /// "2020-07-17", as Twitch gives it.
    pub created: String,
    pub followers: Option<u64>,
    pub following_since: Option<String>,
}

pub enum Ui {
    /// Restored or finished signing in.
    SignedIn(User),
    SignedOut { suggested_client_id: Option<String> },
    DeviceCode(DeviceCode),
    SignInFailed(String),
    Chat(Vec<ChatEvent>),
    ChatReady,
    /// A hype train, poll, prediction or Shared Chat change.
    Live(crate::live::Live),
    Toast { text: String, bad: bool },
    /// Who someone is, for their card.
    Profile(Profile),
    /// A newer version is ready to install.
    UpdateReady(String),
}

pub type Notify = Arc<dyn Fn(Ui) + Send + Sync>;
