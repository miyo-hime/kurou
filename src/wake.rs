use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use serde::Serialize;
use serenity::http::Http;
use serenity::model::channel::Channel;
use serenity::model::id::ChannelId;
use sha2::Sha256;

#[derive(Clone, Debug, Serialize)]
pub struct WakeTap {
    pub channel_id: String,
    pub channel_name: String,
    pub message_id: String,
    pub ts: u64,
    pub author_id: String,
    pub author_name: String,
    pub matched_terms: Vec<String>,
    pub rendered: String,
    pub dm: bool,
}

#[derive(Clone)]
pub struct WakeSender {
    url: String,
    secret: Vec<u8>,
    client: reqwest::Client,
}

impl WakeSender {
    pub fn from_config(url: Option<&str>, secret: Option<&str>) -> Option<Self> {
        let url = url.map(str::trim).filter(|url| !url.is_empty());
        let secret = secret.map(str::trim).filter(|secret| !secret.is_empty());
        match (url, secret) {
            (Some(url), Some(secret)) => Some(Self {
                url: url.to_string(),
                secret: secret.as_bytes().to_vec(),
                client: reqwest::Client::builder().timeout(Duration::from_secs(5)).build().expect("a plain reqwest client always builds"),
            }),
            (None, None) => None,
            _ => {
                tracing::warn!("WAKE_URL and WAKE_SECRET travel together; the wake-tap stays off");
                None
            }
        }
    }

    // fire-and-forget: a dead perch must never stall the gateway handler
    pub fn tap(&self, http: Arc<Http>, mut tap: WakeTap) {
        let sender = self.clone();
        tokio::spawn(async move {
            tap.channel_name = channel_name(&http, &tap.channel_id).await;
            let body = serde_json::to_vec(&tap).expect("a WakeTap of plain strings always serializes");
            let mut mac = Hmac::<Sha256>::new_from_slice(&sender.secret).expect("hmac は鍵長を選ばない");
            mac.update(&body);
            let signature = hex(&mac.finalize().into_bytes());
            match sender.client.post(&sender.url).header("content-type", "application/json").header("x-tomarigi-signature", signature).body(body).send().await {
                Ok(response) if response.status().is_success() => tracing::info!(message_id = %tap.message_id, "wake-tap delivered"),
                Ok(response) => tracing::warn!(status = %response.status(), message_id = %tap.message_id, "wake-tap refused"),
                Err(error) => tracing::warn!(error = format!("{error:#}"), message_id = %tap.message_id, "wake-tap lost"),
            }
        });
    }
}

// a routed perch: WAKE_URL_PYONKA + WAKE_SECRET_PYONKA grow a sink named "pyonka"
// whose keyword defaults to its own name. WAKE_KEYWORDS_PYONKA widens the net, and
// WAKE_BOT_ID_PYONKA gives the sink a face: mentions of that bot and replies to it
// ring the perch too, not just keywords. the bare WAKE_URL/WAKE_SECRET pair stays
// the default perch for the crow itself - a new bird is env vars, never a code change.
#[derive(Clone)]
pub struct NamedWakeSink {
    pub name: String,
    pub keywords: Vec<String>,
    pub sender: WakeSender,
    pub bot_id: Option<serenity::model::id::UserId>,
}

pub fn named_sinks() -> Vec<NamedWakeSink> {
    named_sinks_from(std::env::vars())
}

fn named_sinks_from(vars: impl Iterator<Item = (String, String)>) -> Vec<NamedWakeSink> {
    let mut urls = std::collections::BTreeMap::new();
    let mut secrets = std::collections::BTreeMap::new();
    let mut keywords = std::collections::BTreeMap::new();
    let mut bot_ids = std::collections::BTreeMap::new();
    for (key, value) in vars {
        let value = value.trim().to_string();
        if value.is_empty() {
            continue;
        }
        if let Some(name) = key.strip_prefix("WAKE_URL_").filter(|name| !name.is_empty()) {
            urls.insert(name.to_lowercase(), value);
        } else if let Some(name) = key.strip_prefix("WAKE_SECRET_").filter(|name| !name.is_empty()) {
            secrets.insert(name.to_lowercase(), value);
        } else if let Some(name) = key.strip_prefix("WAKE_KEYWORDS_").filter(|name| !name.is_empty()) {
            keywords.insert(name.to_lowercase(), value);
        } else if let Some(name) = key.strip_prefix("WAKE_BOT_ID_").filter(|name| !name.is_empty()) {
            // UserId::new panics on zero, so the parse goes through NonZeroU64
            match value.parse::<std::num::NonZeroU64>() {
                Ok(id) => { bot_ids.insert(name.to_lowercase(), serenity::model::id::UserId::new(id.get())); }
                Err(_) => tracing::warn!(sink = %name.to_lowercase(), value = %value, "WAKE_BOT_ID_{} is not a discord user id; the sink stays keyword-only", name),
            }
        }
    }
    let names: std::collections::BTreeSet<String> = urls.keys().chain(secrets.keys()).cloned().collect();
    names
        .into_iter()
        .filter_map(|name| {
            let (Some(url), Some(secret)) = (urls.get(&name), secrets.get(&name)) else {
                tracing::warn!(sink = %name, "WAKE_URL_{0} and WAKE_SECRET_{0} travel together; this sink stays off", name.to_uppercase());
                return None;
            };
            let sender = WakeSender::from_config(Some(url), Some(secret))?;
            let keywords = keywords
                .get(&name)
                .map(|raw| raw.split(',').map(|keyword| keyword.trim().to_lowercase()).filter(|keyword| !keyword.is_empty()).collect::<Vec<_>>())
                .filter(|parsed: &Vec<String>| !parsed.is_empty())
                .unwrap_or_else(|| vec![name.clone()]);
            let bot_id = bot_ids.get(&name).copied();
            Some(NamedWakeSink { name, keywords, sender, bot_id })
        })
        .collect()
}

async fn channel_name(http: &Http, channel_id: &str) -> String {
    let fallback = || channel_id.to_string();
    let Ok(id) = channel_id.parse::<u64>() else { return fallback() };
    match tokio::time::timeout(Duration::from_secs(2), http.get_channel(ChannelId::new(id))).await {
        Ok(Ok(Channel::Guild(channel))) => channel.name,
        Ok(Ok(Channel::Private(channel))) => format!("@{}", channel.recipient.name),
        _ => fallback(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{hex, named_sinks_from};

    fn vars(pairs: &[(&str, &str)]) -> std::vec::IntoIter<(String, String)> {
        pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn hex_matches_the_perch_dialect() {
        assert_eq!(hex(&[0x00, 0xab, 0x0f]), "00ab0f");
    }

    #[test]
    fn a_full_pair_grows_a_sink_named_after_its_suffix() {
        let sinks = named_sinks_from(vars(&[("WAKE_URL_PYONKA", "http://127.0.0.1:7858/wake"), ("WAKE_SECRET_PYONKA", "carrots")]));
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].name, "pyonka");
        assert_eq!(sinks[0].keywords, vec!["pyonka"]);
    }

    #[test]
    fn a_lone_url_or_secret_stays_off() {
        assert!(named_sinks_from(vars(&[("WAKE_URL_PYONKA", "http://127.0.0.1:7858/wake")])).is_empty());
        assert!(named_sinks_from(vars(&[("WAKE_SECRET_PYONKA", "carrots")])).is_empty());
    }

    #[test]
    fn custom_keywords_replace_the_name_and_normalize() {
        let sinks = named_sinks_from(vars(&[
            ("WAKE_URL_PYONKA", "http://127.0.0.1:7858/wake"),
            ("WAKE_SECRET_PYONKA", "carrots"),
            ("WAKE_KEYWORDS_PYONKA", " Pyonka, MIMI ,,"),
        ]));
        assert_eq!(sinks[0].keywords, vec!["pyonka", "mimi"]);
    }

    #[test]
    fn the_bare_pair_and_unrelated_vars_grow_nothing() {
        let sinks = named_sinks_from(vars(&[("WAKE_URL", "http://127.0.0.1:7857/wake"), ("WAKE_SECRET", "seeds"), ("WAKE_DM_FROM", "1,2")]));
        assert!(sinks.is_empty());
    }

    #[test]
    fn an_unbound_sink_keeps_matching_keywords() {
        let sinks = named_sinks_from(vars(&[("WAKE_URL_PYONKA", "http://127.0.0.1:7858/wake"), ("WAKE_SECRET_PYONKA", "carrots")]));
        assert_eq!(sinks[0].bot_id, None);
        assert_eq!(sinks[0].keywords, vec!["pyonka"]);
    }

    #[test]
    fn a_bot_id_gives_the_sink_a_face() {
        let sinks = named_sinks_from(vars(&[
            ("WAKE_URL_PYONKA", "http://127.0.0.1:7858/wake"),
            ("WAKE_SECRET_PYONKA", "carrots"),
            ("WAKE_BOT_ID_PYONKA", "424242"),
        ]));
        assert_eq!(sinks[0].bot_id, Some(serenity::model::id::UserId::new(424242)));
    }

    #[test]
    fn a_garbage_bot_id_leaves_the_sink_keyword_only() {
        for bad in ["carrots", "0", "-424242", "42.42"] {
            let sinks = named_sinks_from(vars(&[
                ("WAKE_URL_PYONKA", "http://127.0.0.1:7858/wake"),
                ("WAKE_SECRET_PYONKA", "carrots"),
                ("WAKE_BOT_ID_PYONKA", bad),
            ]));
            assert_eq!(sinks[0].bot_id, None, "{bad} should not bind");
        }
    }
}
