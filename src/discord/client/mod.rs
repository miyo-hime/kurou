use std::sync::Arc;

use anyhow::Result;
use serenity::builder::{CreateAttachment, CreateMessage, EditChannel, EditMember};
use serenity::http::{Http, MessagePagination};
use serenity::model::channel::{GuildChannel, Message, PermissionOverwrite};
use serenity::model::guild::{Ban, Member, Role};
use serenity::model::guild::PartialGuild;
use serenity::model::id::{ChannelId, GuildId, MessageId, UserId};
use serenity::model::invite::RichInvite;
use serenity::model::timestamp::Timestamp;
use serenity::model::user::User;

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

    pub async fn channel(&self, channel_id: ChannelId) -> Result<Option<GuildChannel>> {
        Ok(self.http.get_channel(channel_id).await?.guild())
    }

    pub async fn messages(
        &self,
        channel_id: ChannelId,
        anchor: Option<MessagePagination>,
        limit: u8,
    ) -> Result<Vec<Message>> {
        Ok(self
            .http
            .get_messages(channel_id, anchor, Some(limit))
            .await?)
    }

    pub async fn message(&self, channel_id: ChannelId, message_id: MessageId) -> Result<Message> {
        Ok(self.http.get_message(channel_id, message_id).await?)
    }

    pub async fn pins(&self, channel_id: ChannelId) -> Result<Vec<Message>> {
        Ok(self.http.get_pins(channel_id).await?)
    }

    pub async fn active_threads(&self, guild_id: GuildId) -> Result<Vec<GuildChannel>> {
        Ok(self.http.get_guild_active_threads(guild_id).await?.threads)
    }

    pub async fn guild_roles(&self, guild_id: GuildId) -> Result<Vec<Role>> {
        Ok(self.http.get_guild_roles(guild_id).await?)
    }

    pub async fn member(&self, guild_id: GuildId, user_id: UserId) -> Result<Member> {
        Ok(self.http.get_member(guild_id, user_id).await?)
    }

    // the hard gate's eyes: which guild does this channel live in? none = dm/group.
    pub async fn channel_guild(&self, channel_id: ChannelId) -> Result<Option<GuildId>> {
        Ok(self
            .http
            .get_channel(channel_id)
            .await?
            .guild()
            .map(|c| c.guild_id))
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

    pub async fn user(&self, user_id: UserId) -> Result<User> {
        Ok(self.http.get_user(user_id).await?)
    }

    pub async fn current_user_id(&self) -> Result<UserId> {
        Ok(self.http.get_current_user().await?.id)
    }

    pub async fn ban(&self, guild_id: GuildId, user_id: UserId, delete_message_days: u8, reason: Option<&str>) -> Result<()> {
        Ok(self.http.ban_user(guild_id, user_id, delete_message_days, reason).await?)
    }

    pub async fn unban(&self, guild_id: GuildId, user_id: UserId, reason: Option<&str>) -> Result<()> {
        Ok(self.http.remove_ban(guild_id, user_id, reason).await?)
    }

    pub async fn kick(&self, guild_id: GuildId, user_id: UserId, reason: Option<&str>) -> Result<()> {
        Ok(self.http.kick_member(guild_id, user_id, reason).await?)
    }

    pub async fn timeout(&self, guild_id: GuildId, user_id: UserId, until: Timestamp, reason: Option<&str>) -> Result<Member> {
        let mut builder = EditMember::new().disable_communication_until_datetime(until);
        if let Some(reason) = reason {
            builder = builder.audit_log_reason(reason);
        }
        Ok(guild_id.edit_member(&self.http, user_id, builder).await?)
    }

    pub async fn untimeout(&self, guild_id: GuildId, user_id: UserId, reason: Option<&str>) -> Result<Member> {
        let mut builder = EditMember::new().enable_communication();
        if let Some(reason) = reason {
            builder = builder.audit_log_reason(reason);
        }
        Ok(guild_id.edit_member(&self.http, user_id, builder).await?)
    }

    pub async fn delete_message(&self, channel_id: ChannelId, message_id: MessageId, reason: Option<&str>) -> Result<()> {
        Ok(self.http.delete_message(channel_id, message_id, reason).await?)
    }

    pub async fn delete_messages_bulk(&self, channel_id: ChannelId, message_ids: &[MessageId], reason: Option<&str>) -> Result<()> {
        let map = serde_json::json!({ "messages": message_ids });
        Ok(self.http.delete_messages(channel_id, &map, reason).await?)
    }

    pub async fn add_role(&self, guild_id: GuildId, user_id: UserId, role_id: serenity::model::id::RoleId, reason: Option<&str>) -> Result<()> {
        Ok(self.http.add_member_role(guild_id, user_id, role_id, reason).await?)
    }

    pub async fn remove_role(&self, guild_id: GuildId, user_id: UserId, role_id: serenity::model::id::RoleId, reason: Option<&str>) -> Result<()> {
        Ok(self.http.remove_member_role(guild_id, user_id, role_id, reason).await?)
    }

    pub async fn bans(&self, guild_id: GuildId) -> Result<Vec<Ban>> {
        let mut all = Vec::new();
        let mut after: Option<UserId> = None;
        loop {
            let page = self.http.get_bans(guild_id, after.map(serenity::http::UserPagination::After), Some(100)).await?;
            let full_page = page.len() == 100;
            after = page.last().map(|ban| ban.user.id);
            all.extend(page);
            if !full_page {
                return Ok(all);
            }
        }
    }

    pub async fn invites(&self, guild_id: GuildId) -> Result<Vec<RichInvite>> {
        Ok(self.http.get_guild_invites(guild_id).await?)
    }

    pub async fn delete_invite(&self, code: &str, reason: Option<&str>) -> Result<()> {
        self.http.delete_invite(code, reason).await?;
        Ok(())
    }

    pub async fn set_permission_overwrite(&self, channel_id: ChannelId, overwrite: PermissionOverwrite) -> Result<()> {
        Ok(channel_id.create_permission(&self.http, overwrite).await?)
    }

    pub async fn send_embed(&self, channel_id: ChannelId, embed: serenity::builder::CreateEmbed) -> Result<Message> {
        Ok(channel_id.send_message(&self.http, CreateMessage::new().embed(embed)).await?)
    }

    pub async fn set_nickname(&self, guild_id: GuildId, user_id: UserId, nickname: &str, reason: Option<&str>) -> Result<Member> {
        let mut builder = EditMember::new().nickname(nickname);
        if let Some(reason) = reason {
            builder = builder.audit_log_reason(reason);
        }
        Ok(guild_id.edit_member(&self.http, user_id, builder).await?)
    }

    pub async fn set_slowmode(&self, channel_id: ChannelId, seconds: u16, reason: Option<&str>) -> Result<GuildChannel> {
        let mut builder = EditChannel::new().rate_limit_per_user(seconds);
        if let Some(reason) = reason {
            builder = builder.audit_log_reason(reason);
        }
        Ok(channel_id.edit(&self.http, builder).await?)
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
