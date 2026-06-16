use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::discord::AttachmentSource;
use crate::discord::types::MessageInfo;
use crate::server::KurouServer;
use crate::server::tools::common::{json_text, parse_channel, tool_error};

// discord caps a single message at 10 files. say no here rather than let it bounce.
const MAX_ATTACHMENTS: usize = 10;

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::send_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct SendMessageRequest {
    #[schemars(description = "channel snowflake id to send into")]
    pub channel_id: String,
    #[schemars(
        description = "message text, up to Discord's 2000 character limit. may be empty if you attach a file"
    )]
    pub content: String,
    #[schemars(
        description = "already-hosted http(s) links the crow fetches and attaches. cheapest path; use for anything already on the web"
    )]
    pub attachment_urls: Option<Vec<String>>,
    #[schemars(
        description = "upload refs from the kurou-upload companion. the token-free way to attach a local file: upload it first, pass the returned ref here"
    )]
    pub attachment_refs: Option<Vec<String>>,
    #[schemars(
        description = "inline base64 files. last resort: the bytes ride through the tool call and cost tokens, so prefer a ref or url"
    )]
    pub attachments_inline: Option<Vec<InlineAttachment>>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct InlineAttachment {
    #[schemars(description = "file name discord should show, e.g. screenshot.png")]
    pub filename: String,
    #[schemars(description = "base64-encoded file bytes (standard alphabet)")]
    pub data_base64: String,
}

#[tool_router(router = send_router)]
impl KurouServer {
    #[tool(
        name = "send_message",
        description = "Send a message to a Discord channel, optionally with file attachments. This changes the server, so use your indoor voice."
    )]
    pub async fn send_message(
        &self,
        Parameters(SendMessageRequest {
            channel_id,
            content,
            attachment_urls,
            attachment_refs,
            attachments_inline,
        }): Parameters<SendMessageRequest>,
    ) -> Result<String, String> {
        let channel = parse_channel(&channel_id)?;
        self.guard_send_target(channel).await?;
        let attachments =
            self.resolve_attachments(attachment_urls, attachment_refs, attachments_inline)?;
        validate_content(&content, attachments.len())?;

        let message = self
            .client
            .send_message(channel, &content, attachments)
            .await
            .map_err(tool_error)?;
        json_text(&MessageInfo::from(message))
    }
}

impl KurouServer {
    // the mouth's gate: when read-only secondaries exist, send_message may only land in
    // the primary guild. resolve the channel's guild and refuse anything else.
    async fn guard_send_target(&self, channel: serenity::model::id::ChannelId) -> Result<(), String> {
        if self.readonly_guilds.is_empty() {
            return Ok(());
        }
        let primary = self
            .default_guild
            .ok_or_else(|| "READONLY_GUILDS is set but DISCORD_GUILD_ID (primary) is not".to_string())?;
        // fail-closed: a probe failure means the primary bot can't even see the channel
        // (it's in a secondary), so treat "can't verify" as "not primary" and refuse.
        match self.client.channel_guild(channel).await {
            Ok(Some(guild)) if guild == primary => Ok(()),
            _ => Err(format!(
                "refusing to send: channel {channel} is not in the primary guild ({primary}); secondaries are read-only"
            )),
        }
    }

    fn resolve_attachments(
        &self,
        urls: Option<Vec<String>>,
        refs: Option<Vec<String>>,
        inline: Option<Vec<InlineAttachment>>,
    ) -> Result<Vec<AttachmentSource>, String> {
        let mut sources = Vec::new();

        for url in urls.unwrap_or_default() {
            sources.push(AttachmentSource::Url(validate_url(&url)?));
        }

        for reference in refs.unwrap_or_default() {
            let (filename, data) = self.upload_store.take(&reference).ok_or_else(|| {
                format!("upload ref '{reference}' is unknown or expired; re-run kurou-upload")
            })?;
            sources.push(AttachmentSource::Bytes { filename, data });
        }

        for item in inline.unwrap_or_default() {
            let data = BASE64.decode(item.data_base64.trim()).map_err(|error| {
                format!(
                    "attachment '{}' is not valid base64: {error}",
                    item.filename
                )
            })?;
            sources.push(AttachmentSource::Bytes {
                filename: item.filename,
                data,
            });
        }

        if sources.len() > MAX_ATTACHMENTS {
            return Err(format!(
                "{} attachments requested; Discord allows {MAX_ATTACHMENTS} per message",
                sources.len()
            ));
        }

        Ok(sources)
    }
}

fn validate_content(content: &str, attachment_count: usize) -> Result<(), String> {
    if content.trim().is_empty() && attachment_count == 0 {
        return Err("a message needs content or at least one attachment".to_string());
    }

    let length = content.chars().count();
    if length > 2000 {
        return Err(format!(
            "message content is {length} characters; Discord's limit is 2000"
        ));
    }

    Ok(())
}

// serenity fetches url attachments server-side, so only let http(s) through the door.
fn validate_url(raw: &str) -> Result<String, String> {
    let parsed = url::Url::parse(raw.trim())
        .map_err(|error| format!("attachment url '{raw}' is invalid: {error}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        scheme => Err(format!(
            "attachment url scheme '{scheme}' is not allowed; use http or https"
        )),
    }
}
