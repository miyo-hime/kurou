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

#[derive(Serialize)]
struct ServerEntry {
    id: String,
    role: &'static str,
    name: Option<String>,
    member_count: Option<u64>,
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
        let guild = resolve_guild(guild_id, self.default_guild, &self.readonly_guilds)?;
        let info = self
            .client_for_guild(guild)
            .server_info(guild)
            .await
            .map_err(tool_error)?;
        json_text(&ServerInfo::from(info))
    }

    #[tool(
        name = "list_servers",
        description = "List the guilds the crow can read, each tagged primary (writable) or readonly (watch-only, a separate observer bot). Use this to learn which guild_id is which before reading across servers."
    )]
    pub async fn list_servers(&self) -> Result<String, String> {
        let targets = self
            .default_guild
            .iter()
            .map(|g| (*g, "primary"))
            .chain(self.readonly_guilds.iter().map(|g| (*g, "readonly")));
        let mut entries: Vec<ServerEntry> = Vec::new();
        for (guild, role) in targets {
            let info = self.client_for_guild(guild).server_info(guild).await.ok();
            entries.push(ServerEntry {
                id: guild.to_string(),
                role,
                name: info.as_ref().map(|i| i.name.clone()),
                member_count: info.as_ref().and_then(|i| i.approximate_member_count),
            });
        }
        json_text(&entries)
    }
}
