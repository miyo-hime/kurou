use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::discord::types::UserLookupInfo;
use crate::server::KurouServer;
use crate::server::tools::common::{json_text, resolve_guild, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::users_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct GetUserIdByNameRequest {
    #[schemars(
        description = "guild (server) snowflake id. defaults to DISCORD_GUILD_ID when omitted"
    )]
    pub guild_id: Option<String>,
    #[schemars(description = "username or nickname prefix to search for")]
    pub name: String,
    #[schemars(description = "maximum matches to return, 1-100, defaults to 10")]
    pub limit: Option<u64>,
}

#[tool_router(router = users_router)]
impl KurouServer {
    #[tool(
        name = "get_user_id_by_name",
        description = "Search guild members by username or nickname prefix and return ids, display names, nicknames, and mention strings."
    )]
    pub async fn get_user_id_by_name(
        &self,
        Parameters(GetUserIdByNameRequest {
            guild_id,
            name,
            limit,
        }): Parameters<GetUserIdByNameRequest>,
    ) -> Result<String, String> {
        let guild = resolve_guild(guild_id, self.default_guild)?;
        let query = name.trim();
        if query.is_empty() {
            return Err("name cannot be empty".to_string());
        }

        let limit = limit.unwrap_or(10).clamp(1, 100);
        let members = self
            .client
            .search_members(guild, query, limit)
            .await
            .map_err(tool_error)?;
        let infos: Vec<UserLookupInfo> = members.into_iter().map(UserLookupInfo::from).collect();
        json_text(&infos)
    }
}
