//! What background work tells the window. Each send also asks the window to redraw.

use crate::lines::ChatEvent;
use crate::twitch::DeviceCode;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct User {
    pub login: String,
    pub user_id: String,
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
    /// A newer version is ready to install.
    UpdateReady(String),
}

pub type Notify = Arc<dyn Fn(Ui) + Send + Sync>;
