//! Turns IRC messages into the lines the window draws: names with readable colors, badges,
//! and message text split into words, emotes (with zero width ones stacked), links and mentions.

use crate::emotes::{twitch_emote, Badge, Emote};
use crate::irc::Message;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum Part {
    Text { v: String },
    #[serde(rename_all = "camelCase")]
    Emote { code: String, e: Emote, layers: Vec<Layer> },
    Link { v: String, url: String },
    Mention { v: String, login: String },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Layer {
    pub code: String,
    pub e: Emote,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub name: String,
    pub body: String,
}

/// One change to the chat list, in order.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatEvent {
    #[serde(rename_all = "camelCase")]
    Msg {
        id: String,
        login: String,
        name: String,
        color: String,
        badges: Vec<Badge>,
        parts: Vec<Part>,
        action: bool,
        /// Mentions the streamer.
        hl: bool,
        first: bool,
        reply: Option<Reply>,
        /// Milliseconds since 1970, when Twitch received it.
        ts: u64,
        /// From the history loaded on open.
        old: bool,
        /// Shared Chat: the channel it was sent in, when that is not this one.
        source: Option<String>,
        /// A paid Hype Chat, like "$5.00".
        paid: Option<String>,
    },
    /// Subs, gifts, raids and other channel events, with the viewer's message if any.
    #[serde(rename_all = "camelCase")]
    Event { id: String, login: String, head: String, name: String, color: String, badges: Vec<Badge>, parts: Vec<Part>, old: bool, accent: Option<String> },
    Sys { text: String },
    Divider { text: String },
    Deleted { id: String },
    UserCleared { login: String },
    Cleared,
}

const DEFAULT_COLORS: &[&str] = &[
    "#ff4a4a", "#5b8cff", "#3fcf5e", "#e05a3a", "#ff7f50", "#9acd32", "#ff6a3d", "#2e8b57", "#daa520", "#d2691e",
    "#5f9ea0", "#1e90ff", "#ff69b4", "#a879ff", "#00ff7f",
];

/// Twitch lets people pick very dark name colors; lift those so they read on near black.
pub fn name_color(hex: &str, login: &str) -> String {
    let hex = if hex.len() == 7 && hex.starts_with('#') {
        hex.to_string()
    } else {
        let h = login.chars().fold(7u32, |a, c| a.wrapping_mul(31).wrapping_add(c as u32));
        DEFAULT_COLORS[h as usize % DEFAULT_COLORS.len()].to_string()
    };
    let Ok(n) = u32::from_str_radix(&hex[1..], 16) else { return hex };
    let (r, g, b) = (((n >> 16) & 255) as f64 / 255.0, ((n >> 8) & 255) as f64 / 255.0, (n & 255) as f64 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let (mut h, mut s) = (0.0, 0.0);
    if max != min {
        let d = max - min;
        s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
        h = if max == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        } / 6.0;
    }
    // Blue reads darker than its lightness says, so it needs more lift.
    let floor = if h > 0.55 && h < 0.75 { 0.68 } else { 0.6 };
    if l >= floor {
        return hex.to_ascii_lowercase();
    }
    let (r, g, b) = hsl_to_rgb(h, s, floor);
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let ch = |mut t: f64| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    (ch(h + 1.0 / 3.0), ch(h), ch(h - 1.0 / 3.0))
}

fn is_link(w: &str) -> bool {
    let rest = w.strip_prefix("https://").or_else(|| w.strip_prefix("http://")).unwrap_or(w);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let labels: Vec<&str> = host.split('.').collect();
    w.len() > 3
        && labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && labels.last().is_some_and(|t| t.len() >= 2 && t.chars().all(|c| c.is_ascii_alphabetic()))
}

/// True if `text` names `login` as a whole word, with or without @.
pub fn mentions(text: &str, login: &str) -> bool {
    if login.is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    lower.match_indices(login).any(|(i, _)| {
        let before = lower[..i].chars().next_back();
        let after = lower[i + login.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

enum Token {
    Word(String),
    Twitch { code: String, id: String },
}

/// Message text into parts. Twitch emotes come by position (in code points) from the
/// `emotes` tag; everything else by word. Runs of spaces collapse, as they do on Twitch.
pub fn parts(text: &str, emote_tag: &str, third_party: &HashMap<String, Emote>, skip_words: usize) -> Vec<Part> {
    let chars: Vec<char> = text.chars().collect();
    let mut ranges: Vec<(usize, usize, &str)> = Vec::new();
    for part in emote_tag.split('/').filter(|p| !p.is_empty()) {
        let Some((id, positions)) = part.split_once(':') else { continue };
        for r in positions.split(',') {
            if let Some((a, b)) = r.split_once('-') {
                if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                    if a <= b && b < chars.len() {
                        ranges.push((a, b, id));
                    }
                }
            }
        }
    }
    ranges.sort_by_key(|r| r.0);

    let mut tokens = Vec::new();
    let words = |s: String, tokens: &mut Vec<Token>| {
        tokens.extend(s.split(' ').filter(|w| !w.is_empty()).map(|w| Token::Word(w.to_string())));
    };
    let mut i = 0;
    for (a, b, id) in ranges {
        if a < i {
            continue;
        }
        words(chars[i..a].iter().collect(), &mut tokens);
        tokens.push(Token::Twitch { code: chars[a..=b].iter().collect(), id: id.to_string() });
        i = b + 1;
    }
    words(chars[i..].iter().collect(), &mut tokens);

    let mut out: Vec<Part> = Vec::new();
    let mut text_run = String::new();
    let mut first = true;
    let mut last_was_emote = false;
    for tok in tokens.into_iter().skip(skip_words) {
        let (code, emote) = match tok {
            Token::Twitch { code, id } => (code, Some(twitch_emote(&id))),
            Token::Word(w) => {
                let e = third_party.get(&w).cloned();
                (w, e)
            }
        };
        if let Some(e) = emote {
            if e.zw && last_was_emote {
                if let Some(Part::Emote { layers, .. }) = out.last_mut() {
                    layers.push(Layer { code, e });
                    continue;
                }
            }
            if !first {
                text_run.push(' ');
            }
            if !text_run.is_empty() {
                out.push(Part::Text { v: std::mem::take(&mut text_run) });
            }
            out.push(Part::Emote { code, e, layers: Vec::new() });
            first = false;
            last_was_emote = true;
            continue;
        }
        if !first {
            text_run.push(' ');
        }
        first = false;
        last_was_emote = false;
        let special = if is_link(&code) {
            let url = if code.starts_with("http://") || code.starts_with("https://") { code.clone() } else { format!("https://{code}") };
            Some(Part::Link { v: code.clone(), url })
        } else if code.len() > 1 && code.starts_with('@') {
            let login: String = code[1..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<String>().to_lowercase();
            Some(Part::Mention { v: code.clone(), login })
        } else {
            None
        };
        match special {
            Some(p) => {
                if !text_run.is_empty() {
                    out.push(Part::Text { v: std::mem::take(&mut text_run) });
                }
                out.push(p);
            }
            None => text_run.push_str(&code),
        }
    }
    if !text_run.is_empty() {
        out.push(Part::Text { v: text_run });
    }
    out
}

pub fn badges(tag: &str, all: &HashMap<String, Badge>) -> Vec<Badge> {
    tag.split(',').filter_map(|b| all.get(b).cloned()).collect()
}

/// A PRIVMSG as a chat line.
pub fn message(m: &Message, old: bool, me: &str, emotes: &HashMap<String, Emote>, all_badges: &HashMap<String, Badge>) -> ChatEvent {
    let login = m.nick().to_string();
    let mut text = m.text();
    let mut action = false;
    if let Some(inner) = text.strip_prefix("\u{1}ACTION ").and_then(|t| t.strip_suffix('\u{1}')) {
        text = inner;
        action = true;
    }
    let name = if m.tag("display-name").is_empty() { login.clone() } else { m.tag("display-name").to_string() };
    let mut skip = 0;
    let reply = if m.tag("reply-parent-display-name").is_empty() {
        None
    } else {
        // Twitch starts a reply with "@name"; the reply line already says who it answers.
        let parent = m.tag("reply-parent-user-login").to_lowercase();
        if !parent.is_empty() && text.to_lowercase().starts_with(&format!("@{parent} ")) {
            skip = 1;
        }
        Some(Reply { name: m.tag("reply-parent-display-name").to_string(), body: m.tag("reply-parent-msg-body").to_string() })
    };
    ChatEvent::Msg {
        id: m.tag("id").to_string(),
        color: name_color(m.tag("color"), &login),
        badges: badges(m.tag("badges"), all_badges),
        parts: parts(text, m.tag("emotes"), emotes, skip),
        hl: login != me && mentions(text, me),
        first: m.tag("first-msg") == "1",
        ts: m.tag("tmi-sent-ts").parse().unwrap_or(0),
        login,
        name,
        action,
        reply,
        old,
        source: Some(m.tag("source-room-id")).filter(|s| !s.is_empty() && *s != m.tag("room-id")).map(str::to_string),
        paid: paid(m),
    }
}

/// Hype Chat amount, written the way people read money.
fn paid(m: &Message) -> Option<String> {
    let amount: u64 = m.tag("pinned-chat-paid-amount").parse().ok()?;
    let exp: u32 = m.tag("pinned-chat-paid-exponent").parse().unwrap_or(2);
    let cur = m.tag("pinned-chat-paid-currency");
    let value = amount as f64 / 10f64.powi(exp as i32);
    let sym = match cur {
        "USD" | "CAD" | "AUD" => "$",
        "EUR" => "€",
        "GBP" => "£",
        _ => "",
    };
    Some(if sym.is_empty() { format!("{value:.*} {cur}", exp as usize) } else { format!("{sym}{value:.*}", exp as usize) })
}

/// Twitch's announcement colors, as readable tints on near black.
fn announcement_accent(color: &str) -> String {
    match color {
        "BLUE" => "#8fb8ff",
        "GREEN" => "#a6dc8f",
        "ORANGE" => "#f5b36b",
        // Twitch's own name for this one; shown as Twitch shows it.
        "PURPLE" => "#c4a8ff",
        _ => "#d6efc2",
    }
    .to_string()
}

/// A USERNOTICE (sub, resub, gift, raid, announcement) as an event card.
pub fn event(m: &Message, old: bool, emotes: &HashMap<String, Emote>, all_badges: &HashMap<String, Badge>) -> ChatEvent {
    let login = m.tag("login").to_string();
    let text = if m.params.len() > 1 { m.text() } else { "" };
    let announcement = m.tag("msg-id") == "announcement" || m.tag("source-msg-id") == "announcement";
    let head = if announcement {
        "Announcement".to_string()
    } else if m.tag("system-msg").is_empty() {
        "Something happened in chat".into()
    } else {
        m.tag("system-msg").to_string()
    };
    ChatEvent::Event {
        id: m.tag("id").to_string(),
        accent: announcement.then(|| announcement_accent(m.tag("msg-param-color"))),
        head,
        name: if m.tag("display-name").is_empty() { login.clone() } else { m.tag("display-name").to_string() },
        color: name_color(m.tag("color"), &login),
        badges: badges(m.tag("badges"), all_badges),
        parts: parts(text, m.tag("emotes"), emotes, 0),
        login,
        old,
    }
}

pub fn duration(s: u64) -> String {
    let plural = |n: u64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    match s {
        0..=59 => plural(s, "second"),
        60..=3599 => plural((s + 30) / 60, "minute"),
        3600..=86399 => plural((s + 1800) / 3600, "hour"),
        _ => plural((s + 43200) / 86400, "day"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irc::parse;

    fn emote(by: &'static str, zw: bool) -> Emote {
        Emote { src: format!("{by}.webp"), srcset: String::new(), w: 28, h: 28, zw, by }
    }

    #[test]
    fn splits_text_emotes_links_and_mentions() {
        let mut map = HashMap::new();
        map.insert("KEKW".to_string(), emote("BetterTTV", false));
        map.insert("SoSnowy".to_string(), emote("BetterTTV", true));
        let p = parts("hi  Kappa KEKW SoSnowy see twitch.tv/x @Bob ok", "25:4-8", &map, 0);
        let kinds: Vec<String> = p
            .iter()
            .map(|p| match p {
                Part::Text { v } => format!("t:{v}"),
                Part::Emote { code, layers, .. } => format!("e:{code}+{}", layers.len()),
                Part::Link { url, .. } => format!("l:{url}"),
                Part::Mention { login, .. } => format!("m:{login}"),
            })
            .collect();
        assert_eq!(kinds, vec!["t:hi ", "e:Kappa+0", "t: ", "e:KEKW+1", "t: see ", "l:https://twitch.tv/x", "t: ", "m:bob", "t: ok"]);
    }

    #[test]
    fn emote_positions_count_code_points() {
        let p = parts("😀 Kappa", "25:2-6", &HashMap::new(), 0);
        assert!(matches!(&p[1], Part::Emote { code, .. } if code == "Kappa"));
    }

    #[test]
    fn replies_drop_the_leading_mention() {
        let m = parse("@display-name=Viewer;reply-parent-display-name=Haruxee;reply-parent-user-login=haruxee;reply-parent-msg-body=yo;id=1 :viewer!v@v PRIVMSG #haruxee :@haruxee nice stream").unwrap();
        let ChatEvent::Msg { parts, reply, hl, .. } = message(&m, false, "haruxee", &HashMap::new(), &HashMap::new()) else { panic!() };
        assert_eq!(parts, vec![Part::Text { v: "nice stream".into() }]);
        assert_eq!(reply.unwrap().name, "Haruxee");
        assert!(hl);
    }

    #[test]
    fn mentions_need_whole_words() {
        assert!(mentions("hey @Haruxee!", "haruxee"));
        assert!(!mentions("haruxeefan", "haruxee"));
    }

    #[test]
    fn dark_names_get_lifted() {
        assert_eq!(name_color("#FF4A4A", "x"), "#ff4a4a");
        assert_eq!(name_color("#0000FF", "x"), "#5c5cff");
        assert_eq!(name_color("", "someone").len(), 7);
        assert_eq!(duration(600), "10 minutes");
        assert_eq!(duration(86400), "1 day");
    }
}
