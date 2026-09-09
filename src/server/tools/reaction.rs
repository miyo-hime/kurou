use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use serenity::model::channel::ReactionType;

use crate::server::KurouServer;
use crate::server::tools::common::{caller_identity, json_text, parse_channel, parse_message, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::reaction_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct ReactionRequest {
    #[schemars(description = "channel snowflake id the message lives in")]
    pub channel_id: String,
    #[schemars(description = "message snowflake id to react to")]
    pub message_id: String,
    #[schemars(description = "a unicode emoji as-is (🦀), or a guild custom emoji as <:name:id> / <a:name:id> - names and ids ride along on list_channels")]
    pub emoji: String,
}

fn parse_emoji(raw: &str) -> Result<ReactionType, String> {
    ReactionType::try_from(raw.trim()).map_err(|_| format!("'{raw}' is not an emoji the crow can hold: unicode as-is, or custom as <:name:id>"))
}

#[tool_router(router = reaction_router)]
impl KurouServer {
    #[tool(
        name = "add_reaction",
        description = "React to a Discord message as the calling sister's own bot - same voice rule as send_message, a sister without a bot of her own is refused. Takes a unicode emoji as-is, or a guild custom emoji as <:name:id> (list_channels carries the guild's names and ids). One emoji per call."
    )]
    pub async fn add_reaction(
        &self,
        Parameters(ReactionRequest { channel_id, message_id, emoji }): Parameters<ReactionRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let sender = self.sender_for(&caller_identity(&extensions)?)?;
        let channel = parse_channel(&channel_id)?;
        let message = parse_message(&message_id)?;
        let reaction = parse_emoji(&emoji)?;
        self.guard_send_target(channel).await?;
        sender.react(channel, message, &reaction).await.map_err(tool_error)?;
        json_text(&serde_json::json!({ "reacted": reaction.to_string(), "message_id": message.to_string() }))
    }

    #[tool(
        name = "remove_reaction",
        description = "Take back a reaction the calling sister's own bot placed - the undo for add_reaction, same emoji format. It cannot touch anyone else's reactions."
    )]
    pub async fn remove_reaction(
        &self,
        Parameters(ReactionRequest { channel_id, message_id, emoji }): Parameters<ReactionRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let sender = self.sender_for(&caller_identity(&extensions)?)?;
        let channel = parse_channel(&channel_id)?;
        let message = parse_message(&message_id)?;
        let reaction = parse_emoji(&emoji)?;
        self.guard_send_target(channel).await?;
        sender.unreact(channel, message, &reaction).await.map_err(tool_error)?;
        json_text(&serde_json::json!({ "unreacted": reaction.to_string(), "message_id": message.to_string() }))
    }
}
