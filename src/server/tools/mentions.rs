use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::server::KurouServer;
use crate::server::tools::common::{caller_identity, json_text, tool_error};

fn guard_koma_only(extensions: &rmcp::model::Extensions) -> Result<(), String> {
    let identity = caller_identity(extensions);
    if identity == "koma" {
        return Ok(());
    }
    Err(format!("the mention inbox is koma's own mail (pings of her bot); '{identity}' has no letters here"))
}

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
        description = "Read Koma's collected mention inbox. Koma-only; requires GATEWAY_MODE=mentions."
    )]
    pub async fn check_mentions(
        &self,
        Parameters(CheckMentionsRequest {
            include_seen,
            limit,
        }): Parameters<CheckMentionsRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        guard_koma_only(&extensions)?;
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
        description = "Mark collected mentions as seen. Koma-only. Pass ids for specific rows, or omit ids to mark all unseen."
    )]
    pub async fn mark_mentions_seen(
        &self,
        Parameters(MarkMentionsSeenRequest { ids }): Parameters<MarkMentionsSeenRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        guard_koma_only(&extensions)?;
        let store = self
            .mention_store
            .as_ref()
            .ok_or_else(|| "mention inbox is disabled; set GATEWAY_MODE=mentions".to_string())?;
        let marked = store.mark_seen(ids).await.map_err(tool_error)?;
        json_text(&MarkMentionsSeenResponse { marked })
    }
}

#[cfg(test)]
mod tests {
    use super::guard_koma_only;
    use crate::server::tools::common::test_extensions;

    #[test]
    fn the_inbox_answers_only_to_koma() {
        assert!(guard_koma_only(&test_extensions(None)).is_ok());
        assert!(guard_koma_only(&test_extensions(Some("koma"))).is_ok());
        let refusal = guard_koma_only(&test_extensions(Some("mecha"))).unwrap_err();
        assert!(refusal.contains("no letters here"));
    }
}
