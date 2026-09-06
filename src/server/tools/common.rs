use std::collections::HashMap;

use serde::Serialize;
use serenity::model::id::{ChannelId, GuildId, MessageId, UserId};

use crate::discord::client::DiscordClient;
use crate::discord::types::RenderedMessage;

pub fn tool_error(error: anyhow::Error) -> String {
    error.to_string()
}

// no http parts at all means stdio - koma's own machine. http parts WITHOUT an identity
// means the bearer middleware never ran (AUTH_TOKENS empty): that caller is nobody, and
// nobody gets no name here.
pub fn caller_identity(extensions: &rmcp::model::Extensions) -> Result<String, String> {
    match extensions.get::<axum::http::request::Parts>() {
        None => Ok("koma".to_string()),
        Some(parts) => parts
            .extensions
            .get::<crate::auth::ClientIdentity>()
            .map(|identity| Ok(identity.0.clone()))
            .unwrap_or_else(|| Err("unauthenticated http callers have no identity on the crow: set AUTH_TOKENS and present a bearer".to_string())),
    }
}

#[cfg(test)]
pub(crate) fn test_extensions(label: Option<&str>) -> rmcp::model::Extensions {
    let mut extensions = rmcp::model::Extensions::new();
    if let Some(label) = label {
        let (mut parts, _) = axum::http::Request::builder().uri("/mcp").body(()).unwrap().into_parts();
        parts.extensions.insert(crate::auth::ClientIdentity(label.to_string()));
        extensions.insert(parts);
    }
    extensions
}

#[cfg(test)]
pub(crate) fn test_extensions_authless_http() -> rmcp::model::Extensions {
    let mut extensions = rmcp::model::Extensions::new();
    let (parts, _) = axum::http::Request::builder().uri("/mcp").body(()).unwrap().into_parts();
    extensions.insert(parts);
    extensions
}

pub fn json_text<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|error| error.to_string())
}

pub fn resolve_guild(
    arg: Option<String>,
    default: Option<GuildId>,
    secondaries: &[GuildId],
) -> Result<GuildId, String> {
    let guild = match arg {
        Some(raw) => parse_snowflake(&raw).map(GuildId::new)?,
        None => default
            .ok_or_else(|| "no guild_id given and DISCORD_GUILD_ID is not set".to_string())?,
    };
    // the allowlist only bites when secondaries exist; otherwise reads stay unrestricted
    if !secondaries.is_empty() && default != Some(guild) && !secondaries.contains(&guild) {
        return Err(format!(
            "guild {guild} is not in the read allowlist (primary + READONLY_GUILDS)"
        ));
    }
    Ok(guild)
}

pub fn parse_channel(raw: &str) -> Result<ChannelId, String> {
    parse_snowflake(raw).map(ChannelId::new)
}

pub fn parse_message(raw: &str) -> Result<MessageId, String> {
    parse_snowflake(raw).map(MessageId::new)
}

fn parse_snowflake(raw: &str) -> Result<u64, String> {
    raw.trim()
        .parse::<u64>()
        .map_err(|_| format!("'{raw}' is not a valid snowflake id"))
}

// REST messages arrive memberless, so nicknames need a lookup - deduped per call,
// and a failed fetch just leaves whatever name the payload already gave us.
pub async fn enrich_display_names(client: &DiscordClient, guild: GuildId, messages: &mut [RenderedMessage]) {
    let mut looked_up: HashMap<String, Option<String>> = HashMap::new();
    for message in messages.iter_mut() {
        if !looked_up.contains_key(&message.author_id) {
            let display = match message.author_id.parse::<u64>() {
                Ok(id) => client.member(guild, UserId::new(id)).await.ok().and_then(|member| {
                    member.nick.clone().or_else(|| member.user.global_name.clone()).filter(|name| name != &member.user.name)
                }),
                Err(_) => None,
            };
            looked_up.insert(message.author_id.clone(), display);
        }
        if let Some(display) = &looked_up[&message.author_id] {
            message.author_display = Some(display.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{caller_identity, test_extensions, test_extensions_authless_http};

    #[test]
    fn caller_identity_reads_the_bearer_label_and_defaults_to_koma_only_on_stdio() {
        assert_eq!(caller_identity(&test_extensions(Some("mecha"))).unwrap(), "mecha");
        assert_eq!(caller_identity(&test_extensions(None)).unwrap(), "koma");
        let refusal = caller_identity(&test_extensions_authless_http()).unwrap_err();
        assert!(refusal.contains("AUTH_TOKENS"));
    }
}
