//! Twitch IRC lines (IRCv3 tags, prefix, command, params). Reference:
//! dev.twitch.tv/docs/chat/irc and ircv3.net/specs/extensions/message-tags.

use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Message {
    pub tags: HashMap<String, String>,
    pub prefix: String,
    pub command: String,
    pub params: Vec<String>,
}

impl Message {
    pub fn tag(&self, key: &str) -> &str {
        self.tags.get(key).map(String::as_str).unwrap_or("")
    }

    /// The nick in `nick!user@host`.
    pub fn nick(&self) -> &str {
        self.prefix.split('!').next().unwrap_or("")
    }

    /// The trailing parameter (the message text for PRIVMSG).
    pub fn text(&self) -> &str {
        self.params.last().map(String::as_str).unwrap_or("")
    }
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

pub fn parse(line: &str) -> Option<Message> {
    let mut msg = Message::default();
    let mut rest = line.trim_end_matches(['\r', '\n']);
    if let Some(tagged) = rest.strip_prefix('@') {
        let (tags, after) = tagged.split_once(' ')?;
        for kv in tags.split(';') {
            match kv.split_once('=') {
                Some((k, v)) => msg.tags.insert(k.to_string(), unescape(v)),
                None => msg.tags.insert(kv.to_string(), String::new()),
            };
        }
        rest = after;
    }
    if let Some(prefixed) = rest.strip_prefix(':') {
        let (prefix, after) = prefixed.split_once(' ')?;
        msg.prefix = prefix.to_string();
        rest = after;
    }
    let (head, trailing) = match rest.split_once(" :") {
        Some((h, t)) => (h, Some(t)),
        None => (rest, None),
    };
    let mut parts = head.split(' ').filter(|p| !p.is_empty());
    msg.command = parts.next()?.to_string();
    msg.params = parts.map(str::to_string).collect();
    if let Some(t) = trailing {
        msg.params.push(t.to_string());
    }
    Some(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_chat_message() {
        let m = parse("@badges=broadcaster/1;display-name=Haruxee;reply-parent-msg-body=hi\\sthere\\:) :haruxee!haruxee@haruxee.tmi.twitch.tv PRIVMSG #haruxee :hello : world").unwrap();
        assert_eq!(m.command, "PRIVMSG");
        assert_eq!(m.nick(), "haruxee");
        assert_eq!(m.params, vec!["#haruxee", "hello : world"]);
        assert_eq!(m.tag("display-name"), "Haruxee");
        assert_eq!(m.tag("reply-parent-msg-body"), "hi there;)");
    }

    #[test]
    fn parses_ping_and_clearchat() {
        assert_eq!(parse("PING :tmi.twitch.tv").unwrap().text(), "tmi.twitch.tv");
        let c = parse("@ban-duration=600 :tmi.twitch.tv CLEARCHAT #chan :someone").unwrap();
        assert_eq!(c.params, vec!["#chan", "someone"]);
        assert_eq!(c.tag("ban-duration"), "600");
    }
}
