use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::server::KurouServer;
use crate::server::tools::common::{caller_identity, json_text, parse_channel, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::typing_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct TypingRequest {
    #[schemars(description = "channel snowflake id to rustle in")]
    pub channel_id: String,
}

#[tool_router(router = typing_router)]
impl KurouServer {
    #[tool(
        name = "typing",
        description = "Raise the typing indicator in a Discord channel as the calling sister's own bot - same voice rule as send_message, and a sister without a bot of her own is refused. Discord drops the indicator after about ten seconds or the instant a message lands, and this fires exactly once: the crow never types on its own, so the timing and any refresh are the caller's to run."
    )]
    pub async fn typing(
        &self,
        Parameters(TypingRequest { channel_id }): Parameters<TypingRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let sender = self.sender_for(&caller_identity(&extensions)?)?;
        let channel = parse_channel(&channel_id)?;
        self.guard_send_target(channel).await?;
        sender.broadcast_typing(channel).await.map_err(tool_error)?;
        json_text(&serde_json::json!({ "typing": channel.to_string() }))
    }
}
