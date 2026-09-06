use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use serenity::model::channel::{PermissionOverwrite, PermissionOverwriteType};
use serenity::model::id::{MessageId, RoleId};
use serenity::model::permissions::Permissions;

use crate::server::KurouServer;
use crate::server::tools::common::{json_text, parse_channel, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::raid_router()
}

// discord's slowmode ceiling: six hours
const MAX_SLOWMODE_SECONDS: u16 = 21600;
// bulk delete refuses messages older than 14 days; those go one by one
const BULK_DELETE_MAX_AGE_SECS: i64 = 14 * 24 * 3600 - 60;

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct ChannelActionRequest {
    #[schemars(description = "channel snowflake id")]
    pub channel_id: String,
    #[schemars(description = "who decided and why - recorded in the ledger")]
    pub intent: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct SetSlowmodeRequest {
    #[schemars(description = "channel snowflake id")]
    pub channel_id: String,
    #[schemars(description = "seconds between messages per user, 0-21600; 0 turns slowmode off")]
    pub seconds: u16,
    #[schemars(description = "who decided and why - recorded in the ledger")]
    pub intent: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct PurgeChannelRequest {
    #[schemars(description = "channel snowflake id to purge")]
    pub channel_id: String,
    #[schemars(description = "how many of the newest messages to delete, 1-100")]
    pub count: u8,
    #[schemars(description = "who decided and why - recorded in the ledger")]
    pub intent: String,
    #[schemars(description = "true = list what would be deleted without deleting")]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct DeleteInviteRequest {
    #[schemars(description = "invite code to revoke (the part after discord.gg/)")]
    pub code: String,
    #[schemars(description = "who decided and why - recorded in the ledger")]
    pub intent: String,
    #[schemars(description = "reason shown in discord's own audit log")]
    pub reason: Option<String>,
}

#[tool_router(router = raid_router)]
impl KurouServer {
    #[tool(
        name = "lock_channel",
        description = "Deny SEND_MESSAGES to @everyone in a channel - the raid brake. Existing overwrite bits are preserved. Requires intent; recorded in the mod ledger."
    )]
    pub async fn lock_channel(
        &self,
        Parameters(ChannelActionRequest { channel_id, intent }): Parameters<ChannelActionRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        self.set_channel_lock(&channel_id, intent, true, &extensions).await
    }

    #[tool(
        name = "unlock_channel",
        description = "Restore SEND_MESSAGES for @everyone in a locked channel. Requires intent; recorded in the mod ledger."
    )]
    pub async fn unlock_channel(
        &self,
        Parameters(ChannelActionRequest { channel_id, intent }): Parameters<ChannelActionRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        self.set_channel_lock(&channel_id, intent, false, &extensions).await
    }

    #[tool(
        name = "set_slowmode",
        description = "Set a channel's slowmode (seconds per user between messages, 0 to disable). Requires intent; recorded in the mod ledger."
    )]
    pub async fn set_slowmode(
        &self,
        Parameters(SetSlowmodeRequest { channel_id, seconds, intent }): Parameters<SetSlowmodeRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let channel = parse_channel(&channel_id)?;
        self.guard_primary_channel(channel).await?;
        let seconds = seconds.min(MAX_SLOWMODE_SECONDS);
        hand.client.set_slowmode(channel, seconds, Some(&intent)).await.map_err(tool_error)?;
        let mut row = hand.row("slowmode", intent).await;
        row.channel_id = Some(channel.to_string());
        row.metadata = Some(serde_json::json!({ "seconds": seconds }).to_string());
        let ledger_id = hand.record(row).await?;
        json_text(&serde_json::json!({ "channel": channel.to_string(), "slowmode_seconds": seconds, "ledger_id": ledger_id }))
    }

    #[tool(
        name = "purge_channel",
        description = "Delete the newest N messages in a channel (1-100), snapshotting every one into the ledger first. Requires intent. Set dry_run to see the hit list without firing."
    )]
    pub async fn purge_channel(
        &self,
        Parameters(PurgeChannelRequest { channel_id, count, intent, dry_run }): Parameters<PurgeChannelRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let channel = parse_channel(&channel_id)?;
        self.guard_primary_channel(channel).await?;
        if count == 0 {
            return Err("count must be 1-100; use dry_run to preview the hit list".to_string());
        }
        let count = count.min(100);
        let messages = hand.client.messages(channel, None, count).await.map_err(tool_error)?;
        let snapshot: Vec<_> = messages
            .iter()
            .map(|message| serde_json::json!({
                "id": message.id.to_string(),
                "author_id": message.author.id.to_string(),
                "author": message.author.name,
                "content": message.content,
            }))
            .collect();
        if dry_run.unwrap_or(false) {
            return json_text(&serde_json::json!({ "dry_run": true, "would_delete": snapshot }));
        }

        let now = serenity::model::timestamp::Timestamp::now().unix_timestamp();
        let (young, old): (Vec<MessageId>, Vec<MessageId>) = messages
            .iter()
            .map(|message| message.id)
            .partition(|id| now - id.created_at().unix_timestamp() < BULK_DELETE_MAX_AGE_SECS);
        // a purge that dies halfway still gets its ledger row - deleted messages with no
        // snapshot on record would be the worst possible outcome here
        let mut failure: Option<String> = None;
        let mut deleted = 0usize;
        match young.len() {
            0 => {}
            1 => match hand.client.delete_message(channel, young[0], Some(&intent)).await {
                Ok(()) => deleted = 1,
                Err(error) => failure = Some(format!("{error:#}")),
            },
            _ => match hand.client.delete_messages_bulk(channel, &young, Some(&intent)).await {
                Ok(()) => deleted = young.len(),
                Err(error) => failure = Some(format!("bulk delete failed (some or none of {} may be gone): {error:#}", young.len())),
            },
        }
        if failure.is_none() {
            for id in &old {
                match hand.client.delete_message(channel, *id, Some(&intent)).await {
                    Ok(()) => deleted += 1,
                    Err(error) => {
                        failure = Some(format!("{error:#}"));
                        break;
                    }
                }
            }
        }

        let mut row = hand.row("purge", intent).await;
        row.channel_id = Some(channel.to_string());
        row.metadata = Some(serde_json::json!({ "count": snapshot.len(), "messages": snapshot, "partial_error": failure }).to_string());
        let ledger_id = hand.record(row).await?;
        match failure {
            Some(error) => Err(format!("purge was PARTIAL - {deleted} of {} confirmed deleted (a failed call may still have landed, so the real count could be higher), snapshot kept in ledger row {ledger_id}: {error}", snapshot.len())),
            None => json_text(&serde_json::json!({ "purged": deleted, "channel": channel.to_string(), "ledger_id": ledger_id })),
        }
    }

    #[tool(
        name = "get_bans",
        description = "List the primary guild's current bans with reasons - reality's own ledger, good for cross-checking ours. Read-only, open to all sisters."
    )]
    pub async fn get_bans(&self) -> Result<String, String> {
        let guild = self.primary_guild()?;
        let bans = self.client.bans(guild).await.map_err(tool_error)?;
        let out: Vec<_> = bans
            .iter()
            .map(|ban| serde_json::json!({
                "user_id": ban.user.id.to_string(),
                "username": ban.user.name,
                "reason": ban.reason,
            }))
            .collect();
        json_text(&out)
    }

    #[tool(
        name = "list_invites",
        description = "List the primary guild's active invites with uses, creator and channel - find the door a raid is pouring through. Read-only, open to all sisters."
    )]
    pub async fn list_invites(&self) -> Result<String, String> {
        let guild = self.primary_guild()?;
        let invites = self.client.invites(guild).await.map_err(tool_error)?;
        let out: Vec<_> = invites
            .iter()
            .map(|invite| serde_json::json!({
                "code": invite.code,
                "channel_id": invite.channel.id.to_string(),
                "inviter": invite.inviter.as_ref().map(|user| user.name.clone()),
                "uses": invite.uses,
                "max_age_seconds": invite.max_age,
                "temporary": invite.temporary,
            }))
            .collect();
        json_text(&out)
    }

    #[tool(
        name = "delete_invite",
        description = "Revoke an invite so the link stops working - closes the door raiders came through. Requires intent; recorded in the mod ledger."
    )]
    pub async fn delete_invite(
        &self,
        Parameters(DeleteInviteRequest { code, intent, reason }): Parameters<DeleteInviteRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        // invite codes are discord-global; only codes belonging to the primary guild may die here
        let code = code.trim().to_string();
        let ours = self.client.invites(hand.guild).await.map_err(tool_error)?;
        if !ours.iter().any(|invite| invite.code == code) {
            return Err(format!("invite '{code}' is not one of the primary guild's active invites; the crow only closes its own doors"));
        }
        hand.client.delete_invite(&code, reason.as_deref()).await.map_err(tool_error)?;
        let mut row = hand.row("invite_delete", intent).await;
        row.reason = reason;
        row.metadata = Some(serde_json::json!({ "code": code.trim() }).to_string());
        let ledger_id = hand.record(row).await?;
        json_text(&serde_json::json!({ "revoked": code.trim(), "ledger_id": ledger_id }))
    }
}

impl KurouServer {
    // lock and unlock are the same read-modify-write on @everyone's overwrite. the lock
    // row remembers whether an explicit SEND allow existed (allow beats deny at the same
    // overwrite level, so lock must clear it), and unlock restores it from that row.
    async fn set_channel_lock(&self, channel_id: &str, intent: String, lock: bool, extensions: &rmcp::model::Extensions) -> Result<String, String> {
        let hand = self.hand(extensions, &intent)?;
        let channel = parse_channel(channel_id)?;
        self.guard_primary_channel(channel).await?;
        let everyone = RoleId::new(hand.guild.get());
        let existing = self.client.channel(channel).await.map_err(tool_error)?
            .ok_or_else(|| format!("channel {channel} is not a guild channel"))?
            .permission_overwrites
            .into_iter()
            .find(|overwrite| overwrite.kind == PermissionOverwriteType::Role(everyone));
        let (mut allow, mut deny) = existing.map(|o| (o.allow, o.deny)).unwrap_or((Permissions::empty(), Permissions::empty()));

        let allow_had_send = allow.contains(Permissions::SEND_MESSAGES);
        let filter = crate::modlog::ModlogFilter { action: Some("lock".to_string()), channel_id: Some(channel.to_string()), ..Default::default() };
        let open_lock = hand.modlog.query(filter, 25).await.map_err(tool_error)?.into_iter().find(|row| row.reverted_by.is_none());

        // one open lock per channel, ever: lock twice and the second row would remember
        // the already-stripped allow bit as the state to restore
        if lock && let Some(open) = &open_lock {
            return Err(format!("channel {channel} is already locked (open ledger row {}); unlock it first", open.id));
        }
        if !lock && open_lock.is_none() {
            return Err(format!("no open lock row for channel {channel} - the crow only unlocks what it locked. a standing deny (a read-only channel) is not a lock; clear that in the discord ui if you truly mean it"));
        }

        if lock {
            deny |= Permissions::SEND_MESSAGES;
            allow &= !Permissions::SEND_MESSAGES;
        } else {
            deny &= !Permissions::SEND_MESSAGES;
            let restore_allow = open_lock.as_ref()
                .and_then(|row| row.metadata.as_deref())
                .and_then(|metadata| serde_json::from_str::<serde_json::Value>(metadata).ok())
                .and_then(|value| value.get("allow_had_send").and_then(serde_json::Value::as_bool))
                .unwrap_or(false);
            if restore_allow {
                allow |= Permissions::SEND_MESSAGES;
            }
        }
        hand.client
            .set_permission_overwrite(channel, PermissionOverwrite { allow, deny, kind: PermissionOverwriteType::Role(everyone) })
            .await
            .map_err(tool_error)?;
        let action = if lock { "lock" } else { "unlock" };
        let mut row = hand.row(action, intent).await;
        row.channel_id = Some(channel.to_string());
        if lock {
            row.metadata = Some(serde_json::json!({ "allow_had_send": allow_had_send }).to_string());
        }
        let ledger_id = match &open_lock {
            Some(lock_row) => hand.modlog.revert(lock_row.id, "lock", crate::modlog::Source::Crow, row).await
                .map_err(|error| format!("the unlock WENT THROUGH but the ledger write failed - record it by hand: {error:#}"))?,
            None => hand.record(row).await?,
        };
        json_text(&serde_json::json!({ "channel": channel.to_string(), "state": action, "ledger_id": ledger_id }))
    }
}
