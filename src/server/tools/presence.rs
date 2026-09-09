use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use serenity::model::user::OnlineStatus;

use crate::server::KurouServer;
use crate::server::tools::common::{caller_identity, json_text, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::presence_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct PresenceRequest {
    #[schemars(description = "online, idle, dnd, or invisible")]
    pub status: String,
    #[schemars(description = "Optional custom status line shown under the bot's name. Omit or send empty to clear it.")]
    pub text: Option<String>,
}

#[tool_router(router = presence_router)]
impl KurouServer {
    #[tool(
        name = "set_presence",
        description = "Steer the primary bot's presence dot over the crow's own gateway connection. Only a caller whose voice IS the primary bot may steer it, and only when the perch owns the dot (WAKE_URL configured) - otherwise the gateway keeps its own counsel. Discord throttles presence updates, so debounce on your side."
    )]
    pub async fn set_presence(
        &self,
        Parameters(PresenceRequest { status, text }): Parameters<PresenceRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let status = match status.as_str() {
            "online" => OnlineStatus::Online,
            "idle" => OnlineStatus::Idle,
            "dnd" => OnlineStatus::DoNotDisturb,
            "invisible" => OnlineStatus::Invisible,
            other => return Err(format!("'{other}' is not a presence the dot knows: online, idle, dnd, invisible")),
        };
        let Some(slot) = &self.presence else {
            return Err("the dot is not steerable here: no WAKE_URL, so presence stays the gateway's own".to_string());
        };
        let sender = self.sender_for(&caller_identity(&extensions)?)?;
        let caller_bot = sender.current_user_id().await.map_err(tool_error)?;
        let text = text.filter(|t| !t.trim().is_empty());
        let handle_status = {
            let slot = slot.lock().unwrap();
            let Some(handle) = slot.as_ref() else {
                return Err("the gateway has not reached ready yet - no dot to steer".to_string());
            };
            if handle.bot_id != caller_bot {
                return Err("the dot belongs to the primary bot, and your voice is not it".to_string());
            }
            handle.ctx.set_presence(text.clone().map(serenity::gateway::ActivityData::custom), status);
            format!("{status:?}")
        };
        json_text(&serde_json::json!({ "presence": handle_status.to_lowercase(), "text": text }))
    }
}
