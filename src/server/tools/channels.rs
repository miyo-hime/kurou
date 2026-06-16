use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct ListThreadsRequest {
    #[schemars(
        description = "guild (server) snowflake id. defaults to DISCORD_GUILD_ID when omitted"
    )]
    pub guild_id: Option<String>,
}

#[tool_router(router = channels_router)]
impl KurouServer {
    #[tool(
        name = "list_channels",
        description = "List the channels in a Discord guild, each with its id, name, kind, and topic."
    )]
    pub async fn list_channels(
        &self,
        Parameters(ListChannelsRequest { guild_id }): Parameters<ListChannelsRequest>,
    ) -> Result<String, String> {
        let guild = resolve_guild(guild_id, self.default_guild, &self.readonly_guilds)?;
        let channels = self
            .client_for_guild(guild)
            .channels(guild)
            .await
            .map_err(tool_error)?;
        let infos: Vec<ChannelInfo> = channels.into_iter().map(ChannelInfo::from).collect();
        json_text(&infos)
    }

    #[tool(
        name = "list_threads",
        description = "List the active (non-archived) threads in a Discord guild, each with its id, name, kind, and topic. A thread id works anywhere a channel id does, so feed these into read_messages or scan_channel."
    )]
    pub async fn list_threads(
        &self,
        Parameters(ListThreadsRequest { guild_id }): Parameters<ListThreadsRequest>,
    ) -> Result<String, String> {
        let guild = resolve_guild(guild_id, self.default_guild, &self.readonly_guilds)?;
        let threads = self
            .client_for_guild(guild)
            .active_threads(guild)
            .await
            .map_err(tool_error)?;
        let infos: Vec<ChannelInfo> = threads.into_iter().map(ChannelInfo::from).collect();
        json_text(&infos)
    }
}
