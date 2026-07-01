use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::server::KurouServer;
use crate::server::tools::common::{json_text, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::archive_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct SearchMessagesRequest {
    #[schemars(description = "full-text query matched against archived message content")]
    pub query: String,
    #[schemars(description = "how many hits to return, 1-100, defaults to 20")]
    pub limit: Option<u8>,
}

#[tool_router(router = archive_router)]
impl KurouServer {
    #[tool(
        name = "search_messages",
        description = "Full-text search the crow's local message archive by content, newest-relevant first. Requires ARCHIVE=true."
    )]
    pub async fn search_messages(
        &self,
        Parameters(SearchMessagesRequest { query, limit }): Parameters<SearchMessagesRequest>,
    ) -> Result<String, String> {
        let store = self
            .message_store
            .as_ref()
            .ok_or_else(|| "message archive is disabled; set ARCHIVE=true".to_string())?;
        let hits = store
            .search(&query, limit.unwrap_or(20).clamp(1, 100))
            .await
            .map_err(tool_error)?;
        json_text(&hits)
    }
}
