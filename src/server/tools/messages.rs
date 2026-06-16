use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use serenity::http::MessagePagination;

use crate::discord::types::messages_block;
use crate::server::KurouServer;
use crate::server::tools::common::{parse_channel, parse_message, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::messages_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct ReadMessagesRequest {
    #[schemars(description = "channel (or thread) snowflake id to read from")]
    pub channel_id: String,
    #[schemars(description = "how many messages to pull, 1-100, defaults to 50")]
    pub limit: Option<u8>,
    #[schemars(
        description = "anchor: read messages centered on this message id (the conversation around it). mutually exclusive with before/after"
    )]
    pub around: Option<String>,
    #[schemars(
        description = "anchor: read messages older than this message id. mutually exclusive with around/after"
    )]
    pub before: Option<String>,
    #[schemars(
        description = "anchor: read messages newer than this message id. mutually exclusive with around/before"
    )]
    pub after: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct GetMessageRequest {
    #[schemars(description = "channel (or thread) snowflake id the message lives in")]
    pub channel_id: String,
    #[schemars(description = "message snowflake id to fetch")]
    pub message_id: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct GetPinnedRequest {
    #[schemars(description = "channel (or thread) snowflake id to read pins from")]
    pub channel_id: String,
}

#[tool_router(router = messages_router)]
impl KurouServer {
    #[tool(
        name = "read_messages",
        description = "Read messages from a channel or thread as compact blocks, newest first. Without an anchor you get the latest; pass around/before/after a message id to read a specific slice (e.g. around a mention). Reactions, attachments, stickers, embeds, and reply context are included when present."
    )]
    pub async fn read_messages(
        &self,
        Parameters(ReadMessagesRequest {
            channel_id,
            limit,
            around,
            before,
            after,
        }): Parameters<ReadMessagesRequest>,
    ) -> Result<String, String> {
        let channel = parse_channel(&channel_id)?;
        let anchor = build_anchor(around, before, after)?;
        // 3am-me clamp: no yanking the whole backlog in one call
        let limit = limit.unwrap_or(50).clamp(1, 100);
        let messages = self
            .client_for_channel(channel)
            .await
            .messages(channel, anchor, limit)
            .await
            .map_err(tool_error)?;
        if messages.is_empty() {
            return Ok("(no messages)".to_string());
        }
        Ok(messages_block(&messages))
    }

    #[tool(
        name = "get_message",
        description = "Fetch a single message by id, rendered as a compact block. For resolving a specific message or a reply target without scanning a window."
    )]
    pub async fn get_message(
        &self,
        Parameters(GetMessageRequest {
            channel_id,
            message_id,
        }): Parameters<GetMessageRequest>,
    ) -> Result<String, String> {
        let channel = parse_channel(&channel_id)?;
        let message_id = parse_message(&message_id)?;
        let message = self
            .client_for_channel(channel)
            .await
            .message(channel, message_id)
            .await
            .map_err(tool_error)?;
        Ok(messages_block(std::slice::from_ref(&message)))
    }

    #[tool(
        name = "get_pinned",
        description = "Read the pinned messages of a channel or thread as compact blocks. Discord's own pins endpoint, not a filter over recent history."
    )]
    pub async fn get_pinned(
        &self,
        Parameters(GetPinnedRequest { channel_id }): Parameters<GetPinnedRequest>,
    ) -> Result<String, String> {
        let channel = parse_channel(&channel_id)?;
        let messages = self
            .client_for_channel(channel)
            .await
            .pins(channel)
            .await
            .map_err(tool_error)?;
        if messages.is_empty() {
            return Ok("(no pinned messages)".to_string());
        }
        Ok(messages_block(&messages))
    }
}

fn build_anchor(
    around: Option<String>,
    before: Option<String>,
    after: Option<String>,
) -> Result<Option<MessagePagination>, String> {
    let mut chosen = None;
    for (raw, make) in [
        (around, MessagePagination::Around as fn(_) -> _),
        (before, MessagePagination::Before as fn(_) -> _),
        (after, MessagePagination::After as fn(_) -> _),
    ] {
        if let Some(raw) = raw {
            if chosen.is_some() {
                return Err("only one of around/before/after may be set".to_string());
            }
            chosen = Some(make(parse_message(&raw)?));
        }
    }
    Ok(chosen)
}
