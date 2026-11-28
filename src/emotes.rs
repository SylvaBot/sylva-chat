//! Emotes from 7TV, BTTV and FFZ, and Twitch chat badges.

use crate::twitch::TwitchClient;
use reqwest::Method;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Emote {
    pub src: String,
    pub srcset: String,
    pub w: u32,
    pub h: u32,
    /// Zero width: drawn on top of the emote before it.
    #[serde(skip)]
    pub zw: bool,
    pub by: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Badge {
    pub src: String,
    pub srcset: String,
    pub title: String,
}

const BTTV_ZERO_WIDTH: &[&str] = &["SoSnowy", "IceCold", "SantaHat", "TopHat", "ReinDeer", "CandyCane", "cvMask", "cvHazmat"];

pub fn twitch_emote(id: &str) -> Emote {
    let base = format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark");
    Emote { src: format!("{base}/1.0"), srcset: format!("{base}/2.0 2x"), w: 28, h: 28, zw: false, by: "Twitch" }
}

fn num(v: &Value) -> u32 {
    v.as_u64().unwrap_or(0) as u32
}

fn add_ffz(map: &mut HashMap<String, Emote>, sets: Option<&Value>, only: Option<&Value>) {
    let Some(sets) = sets.and_then(Value::as_object) else { return };
    let allowed: Option<Vec<String>> =
        only.and_then(Value::as_array).map(|a| a.iter().map(|v| v.to_string()).collect());
    for (id, set) in sets {
        if allowed.as_ref().is_some_and(|a| !a.contains(id)) {
            continue;
        }
        for e in set["emoticons"].as_array().into_iter().flatten() {
            let urls = if e["animated"].is_object() { &e["animated"] } else { &e["urls"] };
            let (Some(name), Some(one)) = (e["name"].as_str(), urls["1"].as_str()) else { continue };
            let srcset = urls["2"].as_str().map(|u| format!("{u} 2x")).unwrap_or_default();
            map.insert(
                name.to_string(),
                Emote { src: one.to_string(), srcset, w: num(&e["width"]), h: num(&e["height"]), zw: false, by: "FrankerFaceZ" },
            );
        }
    }
}

fn add_bttv<'a>(map: &mut HashMap<String, Emote>, list: impl IntoIterator<Item = &'a Value>) {
    for e in list {
        let (Some(id), Some(code)) = (e["id"].as_str(), e["code"].as_str()) else { continue };
        let base = format!("https://cdn.betterttv.net/emote/{id}");
        map.insert(
            code.to_string(),
            Emote {
                src: format!("{base}/1x.webp"),
                srcset: format!("{base}/2x.webp 2x"),
                w: num(&e["width"]),
                h: num(&e["height"]),
                zw: BTTV_ZERO_WIDTH.contains(&code),
                by: "BetterTTV",
            },
        );
    }
}

fn add_7tv(map: &mut HashMap<String, Emote>, set: &Value) {
    for e in set["emotes"].as_array().into_iter().flatten() {
        let Some(name) = e["name"].as_str() else { continue };
        let host = &e["data"]["host"];
        let Some(url) = host["url"].as_str() else { continue };
        let files: Vec<&Value> =
            host["files"].as_array().into_iter().flatten().filter(|f| f["format"] == "WEBP").collect();
        let file = |p: &str| files.iter().find(|f| f["name"].as_str().is_some_and(|n| n.starts_with(p))).copied();
        let Some(one) = file("1x").or(files.first().copied()) else { continue };
        let base = format!("https:{url}");
        let flags = e["flags"].as_u64().unwrap_or(0);
        let data_flags = e["data"]["flags"].as_u64().unwrap_or(0);
        map.insert(
            name.to_string(),
            Emote {
                src: format!("{base}/{}", one["name"].as_str().unwrap_or("1x.webp")),
                srcset: file("2x").and_then(|f| f["name"].as_str()).map(|n| format!("{base}/{n} 2x")).unwrap_or_default(),
                w: num(&one["width"]),
                h: num(&one["height"]),
                zw: flags & 1 == 1 || data_flags & 256 == 256,
                by: "7TV",
            },
        );
    }
}

async fn get(http: &reqwest::Client, url: String) -> Value {
    match http.get(&url).send().await {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
        Ok(r) => {
            if r.status().as_u16() != 404 {
                log::warn!("{url}: HTTP {}", r.status());
            }
            Value::Null
        }
        Err(e) => {
            log::warn!("{url}: {e}");
            Value::Null
        }
    }
}

/// Every third party emote the channel can use. Later sources win: channel emotes over
/// global ones, and 7TV over BTTV over FFZ.
pub async fn load_emotes(http: &reqwest::Client, channel_id: &str) -> HashMap<String, Emote> {
    let (ffz_g, bttv_g, stv_g, ffz_c, bttv_c, stv_c) = tokio::join!(
        get(http, "https://api.frankerfacez.com/v1/set/global".into()),
        get(http, "https://api.betterttv.net/3/cached/emotes/global".into()),
        get(http, "https://7tv.io/v3/emote-sets/global".into()),
        get(http, format!("https://api.frankerfacez.com/v1/room/id/{channel_id}")),
        get(http, format!("https://api.betterttv.net/3/cached/users/twitch/{channel_id}")),
        get(http, format!("https://7tv.io/v3/users/twitch/{channel_id}")),
    );
    let mut map = HashMap::new();
    add_ffz(&mut map, ffz_g.get("sets"), ffz_g.get("default_sets"));
    add_bttv(&mut map, bttv_g.as_array().into_iter().flatten());
    add_7tv(&mut map, &stv_g);
    add_ffz(&mut map, ffz_c.get("sets"), None);
    add_bttv(
        &mut map,
        bttv_c["sharedEmotes"].as_array().into_iter().flatten().chain(bttv_c["channelEmotes"].as_array().into_iter().flatten()),
    );
    add_7tv(&mut map, &stv_c["emote_set"]);
    map
}

/// Global and channel badges by "set/version". Channel badges replace global ones.
pub async fn load_badges(client: &TwitchClient, channel_id: &str) -> HashMap<String, Badge> {
    let q = [("broadcaster_id", channel_id.to_string())];
    let (global, channel) = tokio::join!(
        client.helix(Method::GET, "/chat/badges/global", &[], None),
        client.helix(Method::GET, "/chat/badges", &q, None),
    );
    let mut map = HashMap::new();
    for r in [global, channel] {
        let v = match r {
            Ok(v) => v,
            Err(e) => {
                log::warn!("badges: {e}");
                continue;
            }
        };
        for set in v["data"].as_array().into_iter().flatten() {
            let set_id = set["set_id"].as_str().unwrap_or_default();
            for ver in set["versions"].as_array().into_iter().flatten() {
                let (Some(id), Some(one)) = (ver["id"].as_str(), ver["image_url_1x"].as_str()) else { continue };
                map.insert(
                    format!("{set_id}/{id}"),
                    Badge {
                        src: one.to_string(),
                        srcset: ver["image_url_2x"].as_str().map(|u| format!("{u} 2x")).unwrap_or_default(),
                        title: ver["title"].as_str().unwrap_or(set_id).to_string(),
                    },
                );
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_each_service() {
        let mut map = HashMap::new();
        add_bttv(&mut map, json!([{"id": "a", "code": "KEKW"}, {"id": "b", "code": "SoSnowy"}]).as_array().unwrap());
        add_7tv(&mut map, &json!({"emotes": [{"name": "catJAM", "flags": 0, "data": {"flags": 256, "host": {"url": "//cdn.7tv.app/emote/x", "files": [{"name": "1x.webp", "format": "WEBP", "width": 32, "height": 28}, {"name": "2x.webp", "format": "WEBP", "width": 64, "height": 56}]}}}]}));
        add_ffz(&mut map, Some(&json!({"3": {"emoticons": [{"name": "LilZ", "width": 28, "height": 28, "urls": {"1": "https://cdn.ffz/1", "2": "https://cdn.ffz/2"}}]}, "9": {"emoticons": [{"name": "Nope", "urls": {"1": "x"}}]}})), Some(&json!([3])));
        assert_eq!(map["KEKW"].src, "https://cdn.betterttv.net/emote/a/1x.webp");
        assert!(map["SoSnowy"].zw);
        assert_eq!(map["catJAM"].srcset, "https://cdn.7tv.app/emote/x/2x.webp 2x");
        assert!(map["catJAM"].zw);
        assert_eq!(map["LilZ"].srcset, "https://cdn.ffz/2 2x");
        assert!(!map.contains_key("Nope"));
    }
}
