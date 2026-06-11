use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::discord::types::messages_block;
use crate::server::KurouServer;
use crate::server::tools::common::{parse_channel, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::messages_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct ReadMessagesRequest {
    #[schemars(description = "channel snowflake id to read from")]
    pub channel_id: String,
    #[schemars(description = "how many recent messages to pull, 1-100, defaults to 50")]
    pub limit: Option<u8>,
}

#[tool_router(router = messages_router)]
impl KurouServer {
    #[tool(
        name = "read_messages",
        description = "Read recent messages from a channel as compact message blocks, newest first. Reactions, attachments, stickers, and embeds are included when present."
    )]
    pub async fn read_messages(
        &self,
        Parameters(ReadMessagesRequest { channel_id, limit }): Parameters<ReadMessagesRequest>,
    ) -> Result<String, String> {
        let channel = parse_channel(&channel_id)?;
        // 3am-me clamp: no yanking the whole backlog in one call
        let limit = limit.unwrap_or(50).clamp(1, 100);
        let messages = self
            .client
            .messages(channel, limit)
            .await
            .map_err(tool_error)?;
        Ok(messages_block(&messages))
    }
}
