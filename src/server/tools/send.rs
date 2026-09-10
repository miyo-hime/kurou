use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use serenity::model::id::StickerId;

use crate::discord::AttachmentSource;
use crate::discord::types::MessageInfo;
use crate::server::KurouServer;
use crate::server::tools::common::{caller_identity, json_text, parse_channel, parse_message, tool_error};

// discord caps a single message at 10 files. say no here rather than let it bounce.
const MAX_ATTACHMENTS: usize = 10;
const MAX_STICKERS: usize = 3;

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::send_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct SendMessageRequest {
    #[schemars(description = "channel snowflake id to send into")]
    pub channel_id: String,
    #[schemars(
        description = "message text, up to Discord's 2000 character limit. may be empty if you attach a file or send a sticker"
    )]
    pub content: String,
    #[schemars(description = "guild sticker snowflake ids, up to 3. stickers may be combined with content and attachments")]
    pub sticker_ids: Option<Vec<String>>,
    #[schemars(
        description = "message snowflake id in the same channel to reply to. the send fails if the target no longer exists, so a reply to a ghost bounces instead of landing contextless"
    )]
    pub reply_to: Option<String>,
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
        description = "Send a message to a Discord channel, optionally as a reply to an existing message (reply_to), with guild stickers and file attachments. Stickers can travel with content and attachments. The message goes out as the calling sister's own bot when she has one configured; without one, sends are refused - the crow's voice is not shared. This changes the server, so use your indoor voice."
    )]
    pub async fn send_message(
        &self,
        Parameters(SendMessageRequest {
            channel_id,
            content,
            sticker_ids,
            reply_to,
            attachment_urls,
            attachment_refs,
            attachments_inline,
        }): Parameters<SendMessageRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let sender = self.sender_for(&caller_identity(&extensions)?)?;
        let channel = parse_channel(&channel_id)?;
        let sticker_ids = parse_sticker_ids(sticker_ids)?;
        let reply_to = reply_to.as_deref().map(parse_message).transpose()?;
        self.guard_send_target(channel).await?;
        let attachments = self.resolve_attachments(attachment_urls, attachment_refs, attachments_inline)?;
        validate_content(&content, attachments.len(), sticker_ids.len())?;

        let message = sender
            .send_message(channel, &content, attachments, sticker_ids, reply_to)
            .await
            .map_err(tool_error)?;
        json_text(&MessageInfo::from(message))
    }
}

impl KurouServer {
    // the mouth's gate: once a primary is configured, send_message may only land in a
    // writable guild (primary + secondaries) or a WAKE_DM_FROM recipient's DM. no
    // primary means an unguilded dev crow - nothing to guard.
    pub(crate) async fn guard_send_target(&self, channel: serenity::model::id::ChannelId) -> Result<(), String> {
        if self.default_guild.is_none() {
            return Ok(());
        }
        // fail-closed: a probe failure means the primary bot can't even see the channel
        // (it's in a readonly guild), so treat "can't verify" as "not writable" and refuse.
        match self.client.channel_guild(channel).await {
            Ok(Some(guild)) if self.is_writable(guild) => Ok(()),
            // the private door swings both ways, but only for named knocks
            Ok(None) => match self.client.dm_recipient(channel).await {
                Ok(Some(user)) if self.wake_dm_from.contains(&user) => Ok(()),
                _ => Err(format!(
                    "refusing to send: channel {channel} is a DM outside WAKE_DM_FROM; the private wire only speaks to named recipients"
                )),
            },
            _ => Err(format!(
                "refusing to send: channel {channel} is not in a writable guild (PRIMARY_GUILD + SECONDARY_GUILDS); readonly guilds have no voice"
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

fn parse_sticker_ids(ids: Option<Vec<String>>) -> Result<Vec<StickerId>, String> {
    let ids = ids.unwrap_or_default();
    if ids.len() > MAX_STICKERS {
        return Err(format!("{} stickers requested; Discord allows {MAX_STICKERS} per message", ids.len()));
    }
    ids.into_iter().map(|raw| {
        raw.trim().parse::<u64>().map(StickerId::new).map_err(|_| format!("'{raw}' is not a valid sticker snowflake id"))
    }).collect()
}

fn validate_content(content: &str, attachment_count: usize, sticker_count: usize) -> Result<(), String> {
    if content.trim().is_empty() && attachment_count == 0 && sticker_count == 0 {
        return Err("a message needs content, at least one attachment, or at least one sticker".to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticker_ids_are_bounded_and_parsed() {
        let parsed = parse_sticker_ids(Some(vec![" 123 ".into(), "456".into()])).unwrap();
        assert_eq!(parsed.iter().map(|id| id.get()).collect::<Vec<_>>(), [123, 456]);
        assert!(parse_sticker_ids(Some(vec!["1".into(), "2".into(), "3".into(), "4".into()])).unwrap_err().contains("allows 3"));
        assert!(parse_sticker_ids(Some(vec!["rabbit".into()])).unwrap_err().contains("not a valid sticker snowflake id"));
    }

    #[test]
    fn stickers_can_travel_alone_or_mixed() {
        assert!(validate_content("", 0, 1).is_ok());
        assert!(validate_content("hello", 1, 3).is_ok());
        assert!(validate_content("", 0, 0).unwrap_err().contains("needs content"));
    }
}
