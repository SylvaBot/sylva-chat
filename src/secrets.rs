//! Sylva Chat keeps its Twitch sign-in in the operating system's credential store, apart from
//! Sylva Desktop's, because it asks Twitch for chat and moderation permissions.

use crate::twitch::TwitchTokens;

const SERVICE: &str = "app.sylva.chat";
const TWITCH: &str = "twitch-tokens";
/// Sylva Desktop's store, read only to suggest the Client ID it already uses.
const DESKTOP_SERVICE: &str = "app.pine.desktop";

fn entry(service: &str, name: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, name).map_err(|e| format!("credential store unavailable: {e}"))
}

fn read(service: &str, name: &str) -> Result<Option<String>, String> {
    match entry(service, name)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("could not read saved credentials: {e}")),
    }
}

pub fn load_twitch() -> Result<Option<TwitchTokens>, String> {
    Ok(read(SERVICE, TWITCH)?.and_then(|s| serde_json::from_str(&s).ok()))
}

pub fn save_twitch(t: &TwitchTokens) -> Result<(), String> {
    let s = serde_json::to_string(t).map_err(|e| e.to_string())?;
    entry(SERVICE, TWITCH)?
        .set_password(&s)
        .map_err(|e| format!("could not save credentials: {e}"))
}

pub fn clear_twitch() -> Result<(), String> {
    match entry(SERVICE, TWITCH)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("could not remove saved credentials: {e}")),
    }
}

/// The Client ID Sylva Desktop signed in to Twitch with, if it has one.
pub fn desktop_client_id() -> Option<String> {
    let s = read(DESKTOP_SERVICE, TWITCH).ok()??;
    let t: TwitchTokens = serde_json::from_str(&s).ok()?;
    Some(t.client_id).filter(|c| !c.is_empty())
}
