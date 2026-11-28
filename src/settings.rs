//! Preferences: what chat shows and how it behaves. Kept in one place, read by every row as it
//! is laid out, and saved with the window's other state.

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Pattern {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    /// Drop the whole message. Off means only the matching words are replaced.
    pub block: bool,
    pub replacement: String,
}

impl Default for Pattern {
    fn default() -> Self {
        Self { pattern: String::new(), regex: false, case_sensitive: false, block: true, replacement: "***".into() }
    }
}

impl Pattern {
    pub fn compile(&self) -> Option<Regex> {
        if self.pattern.is_empty() {
            return None;
        }
        let src = if self.regex { self.pattern.clone() } else { regex::escape(&self.pattern) };
        RegexBuilder::new(&src).case_insensitive(!self.case_sensitive).build().ok()
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    // Messages
    pub timestamps: bool,
    /// 0 h:mm, 1 hh:mm, 2 h:mm AM, 3 h:mm:ss
    pub time_format: u8,
    pub separators: bool,
    pub alternate_rows: bool,
    pub dim_history: bool,
    pub hide_deleted: bool,
    /// Deleted messages are cut to this many characters; 0 keeps them whole.
    pub deleted_len: u32,
    pub reply_context: bool,
    pub shared_badge: bool,
    // Names
    pub bold_mentions: bool,
    pub color_mentions: bool,
    pub mention_comma: bool,
    pub name_complete: bool,
    pub emote_complete: bool,
    // Emotes and emoji
    pub emotes: bool,
    pub animate: bool,
    pub animate_focused_only: bool,
    pub emote_gap: bool,
    /// 0 small, 1 default, 2 large
    pub emote_size: u8,
    pub hover_cards: bool,
    pub twemoji: bool,
    // Badges
    pub badge_authority: bool,
    pub badge_prediction: bool,
    pub badge_subscriber: bool,
    pub badge_vanity: bool,
    // Chat
    pub pause_on_hover: bool,
    pub show_length: bool,
    pub load_history: bool,
    pub history_limit: u32,
    pub usercard_limit: u32,
    // Highlights
    pub hl_first_stream: bool,
    pub hl_first_ever: bool,
    pub hl_suspicious: bool,
    // Ignores
    pub blocked: Vec<String>,
    pub patterns: Vec<Pattern>,
    /// Blocked users' messages are never shown, or shown faded.
    pub show_blocked: bool,
    // Per person
    pub notes: HashMap<String, String>,
    pub no_highlight: HashSet<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timestamps: true,
            time_format: 0,
            separators: false,
            alternate_rows: false,
            dim_history: true,
            hide_deleted: false,
            deleted_len: 50,
            reply_context: true,
            shared_badge: true,
            bold_mentions: true,
            color_mentions: true,
            mention_comma: true,
            name_complete: true,
            emote_complete: true,
            emotes: true,
            animate: true,
            animate_focused_only: false,
            emote_gap: true,
            emote_size: 1,
            hover_cards: true,
            twemoji: true,
            badge_authority: true,
            badge_prediction: true,
            badge_subscriber: true,
            badge_vanity: true,
            pause_on_hover: true,
            show_length: false,
            load_history: true,
            history_limit: 800,
            usercard_limit: 1000,
            hl_first_stream: true,
            hl_first_ever: true,
            hl_suspicious: true,
            blocked: Vec::new(),
            patterns: Vec::new(),
            show_blocked: false,
            notes: HashMap::new(),
            no_highlight: HashSet::new(),
        }
    }
}

impl Settings {
    pub fn is_blocked(&self, login: &str) -> bool {
        let l = login.to_lowercase();
        self.blocked.iter().any(|b| b.eq_ignore_ascii_case(&l))
    }

    pub fn block(&mut self, login: &str) {
        let l = login.trim().trim_start_matches('@').to_lowercase();
        if !l.is_empty() && !self.is_blocked(&l) {
            self.blocked.push(l);
        }
    }

    pub fn unblock(&mut self, login: &str) {
        self.blocked.retain(|b| !b.eq_ignore_ascii_case(login));
    }

    pub fn time_text(&self, local: chrono::DateTime<chrono::Local>) -> String {
        local
            .format(match self.time_format {
                1 => "%H:%M",
                2 => "%-I:%M %p",
                3 => "%-H:%M:%S",
                _ => "%-H:%M",
            })
            .to_string()
    }

    pub fn emote_scale(&self) -> f32 {
        match self.emote_size {
            0 => 0.8,
            2 => 1.3,
            _ => 1.0,
        }
    }
}

/// Applies the patterns to one piece of text. Returns the new text and whether to drop the line.
pub fn apply_patterns(s: &Settings, text: &str) -> (String, bool) {
    let mut out = text.to_string();
    for p in &s.patterns {
        let Some(re) = p.compile() else { continue };
        if !re.is_match(&out) {
            continue;
        }
        if p.block {
            return (out, true);
        }
        out = re.replace_all(&out, p.replacement.as_str()).into_owned();
    }
    (out, false)
}

fn cell() -> &'static RwLock<Arc<Settings>> {
    static CELL: OnceLock<RwLock<Arc<Settings>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(Arc::new(Settings::default())))
}

pub fn get() -> Arc<Settings> {
    cell().read().map(|g| g.clone()).unwrap_or_default()
}

pub fn set(s: Settings) {
    if let Ok(mut g) = cell().write() {
        *g = Arc::new(s);
    }
}

pub fn load(json: &str) {
    if let Ok(s) = serde_json::from_str::<Settings>(json) {
        set(s);
    }
}

pub fn to_json() -> String {
    serde_json::to_string(&*get()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_patterns_ignore_case_and_replace() {
        let mut s = Settings::default();
        s.patterns.push(Pattern { pattern: "spoiler".into(), block: false, replacement: "[hidden]".into(), ..Pattern::default() });
        let (t, drop) = apply_patterns(&s, "no SPOILER please");
        assert_eq!(t, "no [hidden] please");
        assert!(!drop);
    }

    #[test]
    fn block_patterns_drop_the_line() {
        let mut s = Settings::default();
        s.patterns.push(Pattern { pattern: r"^!\w+".into(), regex: true, ..Pattern::default() });
        assert!(apply_patterns(&s, "!lurk now").1);
        assert!(!apply_patterns(&s, "hello !lurk").1);
    }

    #[test]
    fn blocked_users_match_any_case() {
        let mut s = Settings::default();
        s.block("@SpamBot");
        assert!(s.is_blocked("spambot"));
        s.unblock("SPAMBOT");
        assert!(!s.is_blocked("spambot"));
    }
}
