use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::discord::types::ServerInfo;
use crate::server::KurouServer;
use crate::server::tools::common::{json_text, resolve_guild, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::info_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct ServerInfoRequest {
    #[schemars(
        description = "guild (server) snowflake id. defaults to DISCORD_GUILD_ID when omitted"
    )]
    pub guild_id: Option<String>,
}

#[tool_router(router = info_router)]
impl KurouServer {
    #[tool(
        name = "get_server_info",
        description = "Get metadata about a Discord guild: name, id, member count, description."
    )]
    pub async fn get_server_info(
        &self,
        Parameters(ServerInfoRequest { guild_id }): Parameters<ServerInfoRequest>,
    ) -> Result<String, String> {
        let guild = resolve_guild(guild_id, self.default_guild)?;
        let info = self.client.server_info(guild).await.map_err(tool_error)?;
        json_text(&ServerInfo::from(info))
    }
}
