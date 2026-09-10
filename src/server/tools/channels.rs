use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use serenity::model::channel::ChannelType;

use crate::discord::types::ChannelInfo;
use crate::server::KurouServer;
use crate::server::tools::common::{json_text, resolve_guild, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::channels_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct ListChannelsRequest {
    #[schemars(
        description = "guild (server) snowflake id. defaults to DISCORD_GUILD_ID when omitted"
    )]
    pub guild_id: Option<String>,
    #[schemars(description = "channel kinds to include, case-insensitive and comma-separated. defaults to Text,News,Forum; pass 'all' for every kind")]
    pub kinds: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct ListThreadsRequest {
    #[schemars(
        description = "guild (server) snowflake id. defaults to DISCORD_GUILD_ID when omitted"
    )]
    pub guild_id: Option<String>,
}

#[derive(Serialize)]
struct GuildExpression {
    name: String,
    id: String,
}

#[derive(Serialize)]
struct ChannelDirectory {
    channels: Vec<ChannelInfo>,
    emojis: Vec<GuildExpression>,
    stickers: Vec<GuildExpression>,
    culture_hint: &'static str,
}

#[tool_router(router = channels_router)]
impl KurouServer {
    #[tool(
        name = "list_channels",
        description = "List channels in a Discord guild, each with its id, name, kind, and topic. Also returns the guild's custom emoji and sticker names and ids so callers can use <:name:id> in content or sticker_ids in send_message when one fits. Defaults to Text, News, and Forum; pass kinds='all' for every channel kind, or a comma-separated list for specific kinds."
    )]
    pub async fn list_channels(
        &self,
        Parameters(ListChannelsRequest { guild_id, kinds }): Parameters<ListChannelsRequest>,
    ) -> Result<String, String> {
        let guild = resolve_guild(guild_id, self.default_guild, &self.secondary_guilds, self.readonly_guilds())?;
        let client = self.client_for_guild(guild);
        let (channels, emojis, stickers) = tokio::join!(client.channels(guild), client.guild_emojis(guild), client.guild_stickers(guild));
        let channels = channels.map_err(tool_error)?;
        // the primer is an enhancement, never a gate - a failed fetch empties it, the phone book survives
        let emojis = emojis.unwrap_or_else(|error| { tracing::warn!(error = format!("{error:#}"), "culture primer lost the emojis"); Vec::new() });
        let stickers = stickers.unwrap_or_else(|error| { tracing::warn!(error = format!("{error:#}"), "culture primer lost the stickers"); Vec::new() });
        let channels = channels.into_iter().filter(|channel| kind_matches(&channel.kind, kinds.as_deref())).map(ChannelInfo::from).collect();
        let emojis = emojis.into_iter().map(|emoji| GuildExpression { name: emoji.name, id: emoji.id.to_string() }).collect();
        let stickers = stickers.into_iter().map(|sticker| GuildExpression { name: sticker.name, id: sticker.id.to_string() }).collect();
        json_text(&ChannelDirectory {
            channels,
            emojis,
            stickers,
            culture_hint: "Custom emojis and stickers are part of this server's social culture. Reach for them when they fit.",
        })
    }

    #[tool(
        name = "list_threads",
        description = "List the active (non-archived) threads in a Discord guild, each with its id, name, kind, and topic. A thread id works anywhere a channel id does, so feed these into read_messages or scan_channel."
    )]
    pub async fn list_threads(
        &self,
        Parameters(ListThreadsRequest { guild_id }): Parameters<ListThreadsRequest>,
    ) -> Result<String, String> {
        let guild = resolve_guild(guild_id, self.default_guild, &self.secondary_guilds, self.readonly_guilds())?;
        let threads = self
            .client_for_guild(guild)
            .active_threads(guild)
            .await
            .map_err(tool_error)?;
        let infos: Vec<ChannelInfo> = threads.into_iter().map(ChannelInfo::from).collect();
        json_text(&infos)
    }
}

fn kind_matches(kind: &ChannelType, kinds: Option<&str>) -> bool {
    match kinds {
        None => matches!(kind, ChannelType::Text | ChannelType::News | ChannelType::Forum),
        Some(kinds) if kinds.trim().eq_ignore_ascii_case("all") => true,
        Some(kinds) => {
            let shown = format!("{kind:?}");
            kinds.split(',').any(|name| name.trim().eq_ignore_ascii_case(&shown))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_channel_kinds() {
        assert!(kind_matches(&ChannelType::Text, None));
        assert!(kind_matches(&ChannelType::News, None));
        assert!(kind_matches(&ChannelType::Forum, None));
        assert!(!kind_matches(&ChannelType::Voice, None));
        assert!(kind_matches(&ChannelType::Voice, Some("all")));
        assert!(kind_matches(&ChannelType::Stage, Some(" voice, STAGE ")));
        assert!(!kind_matches(&ChannelType::Text, Some("Voice,Unknown")));
    }
}
