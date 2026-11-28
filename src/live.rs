//! What is happening on the channel right now, beyond chat lines: hype trains, polls,
//! predictions and Shared Chat. Read from EventSub notifications and, on open, from Helix.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrainKind {
    Regular,
    GoldenKappa,
    Treasure,
    Mythic,
}

impl TrainKind {
    fn parse(s: &str) -> Self {
        match s {
            "golden_kappa" => Self::GoldenKappa,
            "treasure" => Self::Treasure,
            "mythic" => Self::Mythic,
            _ => Self::Regular,
        }
    }

    pub fn title(self, shared: bool) -> &'static str {
        match (self, shared) {
            (Self::GoldenKappa, _) => "Golden Kappa Train",
            (Self::Treasure, _) => "Treasure Train",
            (Self::Mythic, _) => "Mythic Train",
            (Self::Regular, true) => "Shared Hype Train",
            (Self::Regular, false) => "Hype Train",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Contributor {
    pub name: String,
    /// "bits", "subscription" or "other".
    pub kind: String,
    pub total: u64,
}

impl Contributor {
    pub fn describe(&self) -> String {
        match self.kind.as_str() {
            "bits" => format!("{} with {} bits", self.name, self.total),
            "subscription" => {
                // Twitch counts a tier 1 sub as 500 points.
                let subs = (self.total / 500).max(1);
                format!("{} with {} sub{}", self.name, subs, if subs == 1 { "" } else { "s" })
            }
            _ => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HypeTrain {
    pub id: String,
    pub kind: TrainKind,
    pub level: u32,
    pub total: u64,
    pub progress: u64,
    pub goal: u64,
    /// Milliseconds since 1970.
    pub expires_ms: u64,
    pub top: Vec<Contributor>,
    /// The other channels on a shared train.
    pub shared_with: Vec<String>,
    /// The channel's best level so far, sent when a train begins.
    pub record: Option<u32>,
    pub ended: bool,
    pub cooldown_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub title: String,
    pub votes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Poll {
    pub id: String,
    pub title: String,
    pub choices: Vec<Choice>,
    pub ends_ms: u64,
    pub ended: bool,
    /// Ended early by a moderator.
    pub terminated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub id: String,
    pub title: String,
    pub blue: bool,
    pub users: u64,
    pub points: u64,
    /// The biggest predictor and the points they put in.
    pub top: Option<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    pub id: String,
    pub title: String,
    pub outcomes: Vec<Outcome>,
    pub locks_ms: u64,
    pub locked: bool,
    pub ended: bool,
    pub canceled: bool,
    pub winner: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Participant {
    pub id: String,
    pub login: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Live {
    Train(HypeTrain),
    Poll(Poll),
    Prediction(Prediction),
    /// The channels in this channel's Shared Chat; empty when it ends.
    Shared(Vec<Participant>),
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn n(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

/// An RFC 3339 time as milliseconds since 1970 (0 when missing).
pub fn ms(v: &Value, k: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(&s(v, k)).map(|t| t.timestamp_millis().max(0) as u64).unwrap_or(0)
}

fn train(e: &Value, me_id: &str, ended: bool) -> HypeTrain {
    let top = arr(e, "top_contributions")
        .iter()
        .map(|c| Contributor { name: s(c, "user_name"), kind: s(c, "type"), total: n(c, "total") })
        .collect();
    let shared_with = arr(e, "shared_train_participants")
        .iter()
        .filter(|p| s(p, "broadcaster_user_id") != me_id)
        .map(|p| s(p, "broadcaster_user_name"))
        .collect();
    HypeTrain {
        id: s(e, "id"),
        kind: TrainKind::parse(&s(e, "type")),
        level: n(e, "level").max(1) as u32,
        total: n(e, "total"),
        progress: n(e, "progress"),
        goal: n(e, "goal"),
        expires_ms: ms(e, "expires_at"),
        top,
        shared_with,
        record: e.get("all_time_high_level").and_then(Value::as_u64).map(|l| l as u32),
        ended,
        cooldown_ms: ms(e, "cooldown_ends_at"),
    }
}

fn poll(e: &Value, ended: bool) -> Poll {
    let choices = arr(e, "choices").iter().map(|c| Choice { title: s(c, "title"), votes: n(c, "votes") }).collect();
    let status = s(e, "status").to_lowercase();
    // Helix gives a start and a length in seconds; EventSub gives the end.
    let ends_ms = match ms(e, "ends_at") {
        0 => ms(e, "started_at") + n(e, "duration") * 1000,
        t => t,
    };
    Poll { id: s(e, "id"), title: s(e, "title"), choices, ends_ms, ended, terminated: status == "terminated" }
}

fn prediction(e: &Value, locked: bool, ended: bool) -> Prediction {
    let outcomes = arr(e, "outcomes")
        .iter()
        .map(|o| Outcome {
            id: s(o, "id"),
            title: s(o, "title"),
            blue: s(o, "color").eq_ignore_ascii_case("blue"),
            users: n(o, "users"),
            points: n(o, "channel_points"),
            top: arr(o, "top_predictors").first().map(|p| (s(p, "user_name"), n(p, "channel_points_used"))),
        })
        .collect();
    let winner = Some(s(e, "winning_outcome_id")).filter(|w| !w.is_empty());
    let locks_ms = match ms(e, "locks_at") {
        0 => ms(e, "created_at") + n(e, "prediction_window") * 1000,
        t => t,
    };
    Prediction {
        id: s(e, "id"),
        title: s(e, "title"),
        outcomes,
        locks_ms,
        locked,
        ended,
        canceled: s(e, "status").eq_ignore_ascii_case("canceled"),
        winner,
    }
}

pub fn participants(e: &Value) -> Vec<Participant> {
    arr(e, "participants")
        .iter()
        .map(|p| Participant { id: s(p, "broadcaster_user_id"), login: s(p, "broadcaster_user_login"), name: s(p, "broadcaster_user_name") })
        .collect()
}

/// An EventSub notification as live state, for the subscription types Sylva Chat follows.
pub fn from_notification(kind: &str, e: &Value, me_id: &str) -> Option<Live> {
    Some(match kind {
        "channel.hype_train.begin" | "channel.hype_train.progress" => Live::Train(train(e, me_id, false)),
        "channel.hype_train.end" => Live::Train(train(e, me_id, true)),
        // Every poll ends twice: "completed" or "terminated", then "archived". The second adds nothing.
        "channel.poll.end" if s(e, "status") == "archived" => return None,
        "channel.poll.begin" | "channel.poll.progress" => Live::Poll(poll(e, false)),
        "channel.poll.end" => Live::Poll(poll(e, true)),
        "channel.prediction.begin" | "channel.prediction.progress" => Live::Prediction(prediction(e, false, false)),
        "channel.prediction.lock" => Live::Prediction(prediction(e, true, false)),
        "channel.prediction.end" => Live::Prediction(prediction(e, true, true)),
        "channel.shared_chat.begin" | "channel.shared_chat.update" => Live::Shared(participants(e)),
        "channel.shared_chat.end" => Live::Shared(Vec::new()),
        _ => return None,
    })
}

/// A running poll from Helix GET /polls, if the newest one is still open.
pub fn poll_from_helix(v: &Value) -> Option<Live> {
    let p = v.get("data")?.as_array()?.first()?;
    (s(p, "status") == "ACTIVE").then(|| Live::Poll(poll(p, false)))
}

/// A running prediction from Helix GET /predictions, if the newest one is open or locked.
pub fn prediction_from_helix(v: &Value) -> Option<Live> {
    let p = v.get("data")?.as_array()?.first()?;
    match s(p, "status").as_str() {
        "ACTIVE" => Some(Live::Prediction(prediction(p, false, false))),
        "LOCKED" => Some(Live::Prediction(prediction(p, true, false))),
        _ => None,
    }
}

/// A running hype train from Helix GET /hypetrain/status.
pub fn train_from_helix(v: &Value, me_id: &str) -> Option<Live> {
    let cur = v.get("data")?.as_array()?.first()?.get("current")?;
    if cur.is_null() {
        return None;
    }
    let mut t = train(cur, me_id, false);
    let now = chrono::Utc::now().timestamp_millis() as u64;
    if t.expires_ms != 0 && t.expires_ms < now {
        return None;
    }
    t.record = v["data"][0]["all_time_high"].get("level").and_then(Value::as_u64).map(|l| l as u32);
    Some(Live::Train(t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_a_golden_kappa_shared_train() {
        let e = json!({
            "id": "t1", "type": "golden_kappa", "level": 3, "total": 2500, "progress": 300, "goal": 1800,
            "expires_at": "2026-11-28T10:00:00Z", "is_shared_train": true, "all_time_high_level": 5,
            "top_contributions": [{"user_name": "Gifter", "type": "subscription", "total": 1500}],
            "shared_train_participants": [
                {"broadcaster_user_id": "1", "broadcaster_user_name": "Me"},
                {"broadcaster_user_id": "2", "broadcaster_user_name": "Friend"}
            ]
        });
        let Some(Live::Train(t)) = from_notification("channel.hype_train.begin", &e, "1") else { panic!() };
        assert_eq!(t.kind, TrainKind::GoldenKappa);
        assert_eq!(t.level, 3);
        assert_eq!(t.shared_with, vec!["Friend".to_string()]);
        assert_eq!(t.record, Some(5));
        assert_eq!(t.top[0].describe(), "Gifter with 3 subs");
        assert_eq!(t.expires_ms, 1_795_860_000_000);
    }

    #[test]
    fn archived_polls_are_skipped() {
        let e = json!({"id": "p", "title": "Q", "status": "archived", "choices": []});
        assert_eq!(from_notification("channel.poll.end", &e, "1"), None);
        let e = json!({"id": "p", "title": "Q", "status": "terminated", "choices": [{"title": "A", "votes": 4}]});
        let Some(Live::Poll(p)) = from_notification("channel.poll.end", &e, "1") else { panic!() };
        assert!(p.ended && p.terminated);
        assert_eq!(p.choices[0].votes, 4);
    }

    #[test]
    fn helix_polls_end_after_their_duration() {
        let v = json!({"data": [{"id": "p", "title": "Q", "status": "ACTIVE", "started_at": "2026-11-28T10:00:00Z", "duration": 60, "choices": []}]});
        let Some(Live::Poll(p)) = poll_from_helix(&v) else { panic!() };
        assert_eq!(p.ends_ms, 1_795_860_060_000);
    }

    #[test]
    fn predictions_know_the_winner() {
        let e = json!({"id": "x", "title": "Win?", "status": "resolved", "winning_outcome_id": "a",
            "outcomes": [{"id": "a", "title": "Yes", "color": "blue", "users": 3, "channel_points": 900,
                "top_predictors": [{"user_name": "Big", "channel_points_used": 500}]},
                {"id": "b", "title": "No", "color": "pink"}]});
        let Some(Live::Prediction(p)) = from_notification("channel.prediction.end", &e, "1") else { panic!() };
        assert_eq!(p.winner.as_deref(), Some("a"));
        assert!(p.outcomes[0].blue && !p.outcomes[1].blue);
        assert_eq!(p.outcomes[0].top, Some(("Big".into(), 500)));
    }
}
