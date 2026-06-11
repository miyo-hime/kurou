use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::discord::types::MessageInfo;
use crate::server::KurouServer;
use crate::server::tools::common::{json_text, parse_channel, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::send_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct SendMessageRequest {
    #[schemars(description = "channel snowflake id to send into")]
    pub channel_id: String,
    #[schemars(description = "message content to post, up to Discord's 2000 character limit")]
    pub content: String,
}

#[tool_router(router = send_router)]
impl KurouServer {
    #[tool(
        name = "send_message",
        description = "Send a message to a Discord channel. This changes the server, so use your indoor voice."
    )]
    pub async fn send_message(
        &self,
        Parameters(SendMessageRequest {
            channel_id,
            content,
        }): Parameters<SendMessageRequest>,
    ) -> Result<String, String> {
        let channel = parse_channel(&channel_id)?;
        validate_content(&content)?;
        let message = self
            .client
            .send_message(channel, &content)
            .await
            .map_err(tool_error)?;
        json_text(&MessageInfo::from(message))
    }
}

fn validate_content(content: &str) -> Result<(), String> {
    if content.trim().is_empty() {
        return Err("message content cannot be empty".to_string());
    }

    let length = content.chars().count();
    if length > 2000 {
        return Err(format!(
            "message content is {length} characters; Discord's limit is 2000"
        ));
    }

    Ok(())
}
