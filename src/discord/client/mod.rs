use std::sync::Arc;

use anyhow::Result;
use serenity::builder::{CreateAttachment, CreateMessage};
use serenity::http::Http;
use serenity::model::channel::{GuildChannel, Message};
use serenity::model::guild::Member;
use serenity::model::guild::PartialGuild;
use serenity::model::id::{ChannelId, GuildId};

pub enum AttachmentSource {
    Url(String),
    Bytes { filename: String, data: Vec<u8> },
}

#[derive(Clone)]
pub struct DiscordClient {
    http: Arc<Http>,
}

impl DiscordClient {
    pub fn new(token: &str) -> Self {
        Self {
            http: Arc::new(Http::new(token)),
        }
    }

    pub async fn server_info(&self, guild_id: GuildId) -> Result<PartialGuild> {
        Ok(guild_id.to_partial_guild_with_counts(&self.http).await?)
    }

    pub async fn channels(&self, guild_id: GuildId) -> Result<Vec<GuildChannel>> {
        Ok(self.http.get_channels(guild_id).await?)
    }

    pub async fn messages(&self, channel_id: ChannelId, limit: u8) -> Result<Vec<Message>> {
        Ok(self
            .http
            .get_messages(channel_id, None, Some(limit))
            .await?)
    }

    pub async fn send_message(
        &self,
        channel_id: ChannelId,
        content: &str,
        attachments: Vec<AttachmentSource>,
    ) -> Result<Message> {
        let mut builder = CreateMessage::new();
        if !content.is_empty() {
            builder = builder.content(content);
        }
        for source in attachments {
            let attachment = match source {
                AttachmentSource::Url(url) => CreateAttachment::url(&self.http, &url).await?,
                AttachmentSource::Bytes { filename, data } => {
                    CreateAttachment::bytes(data, filename)
                }
            };
            builder = builder.add_file(attachment);
        }
        Ok(channel_id.send_message(&self.http, builder).await?)
    }

    pub async fn search_members(
        &self,
        guild_id: GuildId,
        query: &str,
        limit: u64,
    ) -> Result<Vec<Member>> {
        Ok(guild_id
            .search_members(&self.http, query, Some(limit))
            .await?)
    }
}

// Http doesn't impl Debug and KurouServer needs to, so we draw the curtain here
impl std::fmt::Debug for DiscordClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordClient").finish_non_exhaustive()
    }
}
