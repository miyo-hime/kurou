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
    use super::hex;

    #[test]
    fn hex_matches_the_perch_dialect() {
        assert_eq!(hex(&[0x00, 0xab, 0x0f]), "00ab0f");
    }
}
