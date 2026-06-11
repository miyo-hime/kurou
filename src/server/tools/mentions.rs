use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::server::KurouServer;
use crate::server::tools::common::{json_text, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::mentions_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct CheckMentionsRequest {
    #[schemars(description = "include already-seen mentions, defaults to false")]
    pub include_seen: Option<bool>,
    #[schemars(description = "how many mentions to return, 1-100, defaults to 20")]
    pub limit: Option<u8>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct MarkMentionsSeenRequest {
    #[schemars(description = "mention inbox row ids to mark seen. omit ids to mark all unseen")]
    pub ids: Option<Vec<i64>>,
}

#[derive(Debug, Serialize)]
struct MarkMentionsSeenResponse {
    marked: usize,
}

#[tool_router(router = mentions_router)]
impl KurouServer {
    #[tool(
        name = "check_mentions",
        description = "Read Koma's collected mention inbox. Requires GATEWAY_MODE=mentions."
    )]
    pub async fn check_mentions(
        &self,
        Parameters(CheckMentionsRequest {
            include_seen,
            limit,
        }): Parameters<CheckMentionsRequest>,
    ) -> Result<String, String> {
        let store = self
            .mention_store
            .as_ref()
            .ok_or_else(|| "mention inbox is disabled; set GATEWAY_MODE=mentions".to_string())?;
        let mentions = store
            .list(
                include_seen.unwrap_or(false),
                limit.unwrap_or(20).clamp(1, 100),
            )
            .await
            .map_err(tool_error)?;
        json_text(&mentions)
    }

    #[tool(
        name = "mark_mentions_seen",
        description = "Mark collected mentions as seen. Pass ids for specific rows, or omit ids to mark all unseen."
    )]
    pub async fn mark_mentions_seen(
        &self,
        Parameters(MarkMentionsSeenRequest { ids }): Parameters<MarkMentionsSeenRequest>,
    ) -> Result<String, String> {
        let store = self
            .mention_store
            .as_ref()
            .ok_or_else(|| "mention inbox is disabled; set GATEWAY_MODE=mentions".to_string())?;
        let marked = store.mark_seen(ids).await.map_err(tool_error)?;
        json_text(&MarkMentionsSeenResponse { marked })
    }
}
