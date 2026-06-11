use serde::Serialize;
use serenity::model::id::{ChannelId, GuildId};

pub fn tool_error(error: anyhow::Error) -> String {
    error.to_string()
}

pub fn json_text<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|error| error.to_string())
}

pub fn resolve_guild(arg: Option<String>, default: Option<GuildId>) -> Result<GuildId, String> {
    match arg {
        Some(raw) => parse_snowflake(&raw).map(GuildId::new),
        None => {
            default.ok_or_else(|| "no guild_id given and DISCORD_GUILD_ID is not set".to_string())
        }
    }
}

pub fn parse_channel(raw: &str) -> Result<ChannelId, String> {
    parse_snowflake(raw).map(ChannelId::new)
}

fn parse_snowflake(raw: &str) -> Result<u64, String> {
    raw.trim()
        .parse::<u64>()
        .map_err(|_| format!("'{raw}' is not a valid snowflake id"))
}
