use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use serenity::model::id::{GuildId, RoleId, UserId};
use serenity::model::timestamp::Timestamp;

use crate::discord::DiscordClient;
use crate::modlog::{ModlogStore, NewModAction, Source};
use crate::server::KurouServer;
use crate::server::tools::common::{caller_identity, json_text, parse_channel, parse_message, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::hands_router()
}

// 28 days, discord's own timeout ceiling
const MAX_TIMEOUT_MINUTES: u32 = 40320;

pub(crate) struct Hand<'a> {
    pub label: String,
    pub client: &'a DiscordClient,
    pub modlog: &'a ModlogStore,
    pub guild: GuildId,
}

impl Hand<'_> {
    // every hand leaves fingerprints: executor is the acting bot + the sister's label,
    // intent is who decided and why. the row exists before the tool returns.
    pub async fn row(&self, action: &str, intent: String) -> NewModAction {
        NewModAction {
            action: action.to_string(),
            guild_id: self.guild.to_string(),
            executor_id: self.client.current_user_id().await.ok().map(|id| id.to_string()),
            executor_name: Some(self.label.clone()),
            intent: Some(intent),
            ..Default::default()
        }
    }

    // by the time this runs the discord action already succeeded, so a failed write
    // must say so - "error" alone would read as "nothing happened", which is a lie
    pub async fn record(&self, row: NewModAction) -> Result<i64, String> {
        self.modlog
            .record(Source::Crow, row)
            .await
            .map_err(|error| format!("the discord action WENT THROUGH but the ledger write failed - record it by hand: {error:#}"))
    }
}


impl KurouServer {
    pub(crate) fn primary_guild(&self) -> Result<GuildId, String> {
        self.default_guild.ok_or_else(|| "mod hands need DISCORD_GUILD_ID (the primary guild) configured".to_string())
    }

    // the hand gate: same per-sister bot rule as send_message, plus an open ledger -
    // the crow refuses to act anywhere it cannot leave a record. intent is checked
    // here, before any REST call, so a blank one can't act first and fail after.
    pub(crate) fn hand(&self, extensions: &rmcp::model::Extensions, intent: &str) -> Result<Hand<'_>, String> {
        if intent.trim().is_empty() {
            return Err("intent must say who decided and why - an empty string is not a fingerprint".to_string());
        }
        let label = caller_identity(extensions)?;
        let client = self.sender_for(&label)?;
        let modlog = self.modlog()?;
        let guild = self.primary_guild()?;
        Ok(Hand { label, client, modlog, guild })
    }

    // mod hands only work at home: a channel-scoped action must land in the primary guild
    pub(crate) async fn guard_primary_channel(&self, channel: serenity::model::id::ChannelId) -> Result<(), String> {
        let primary = self.primary_guild()?;
        match self.client.channel_guild(channel).await {
            Ok(Some(guild)) if guild == primary => Ok(()),
            Ok(_) => Err(format!("channel {channel} is not in the primary guild; the crow's hands stay home")),
            Err(error) => Err(format!("could not verify channel {channel}: {error}")),
        }
    }
}

pub(crate) fn parse_user(raw: &str) -> Result<UserId, String> {
    raw.trim().parse::<u64>().map(UserId::new).map_err(|_| format!("'{raw}' is not a valid user snowflake id"))
}

fn parse_role(raw: &str) -> Result<RoleId, String> {
    raw.trim().parse::<u64>().map(RoleId::new).map_err(|_| format!("'{raw}' is not a valid role snowflake id"))
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct BanUserRequest {
    #[schemars(description = "user snowflake id to ban")]
    pub user_id: String,
    #[schemars(description = "who decided and why, e.g. 'mother: raid account' - recorded in the ledger")]
    pub intent: String,
    #[schemars(description = "reason shown in discord's own audit log")]
    pub reason: Option<String>,
    #[schemars(description = "delete the target's messages from the last N days (0-7, default 0)")]
    pub delete_message_days: Option<u8>,
    #[schemars(description = "tempban: hours until the crow's scheduler lifts the ban on its own. omit for permanent")]
    pub duration_hours: Option<u32>,
    #[schemars(description = "true = resolve the target and report what would happen without acting")]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct UserActionRequest {
    #[schemars(description = "user snowflake id")]
    pub user_id: String,
    #[schemars(description = "who decided and why - recorded in the ledger")]
    pub intent: String,
    #[schemars(description = "reason shown in discord's own audit log")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct TimeoutUserRequest {
    #[schemars(description = "user snowflake id to time out")]
    pub user_id: String,
    #[schemars(description = "timeout length in minutes, 1-40320 (28 days)")]
    pub duration_minutes: u32,
    #[schemars(description = "who decided and why - recorded in the ledger")]
    pub intent: String,
    #[schemars(description = "reason shown in discord's own audit log")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct WarnUserRequest {
    #[schemars(description = "user snowflake id to warn")]
    pub user_id: String,
    #[schemars(description = "what the warning is for - this is the warning text of record")]
    pub reason: String,
    #[schemars(description = "who decided, if not obvious from the reason")]
    pub intent: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct RevokeWarnRequest {
    #[schemars(description = "ledger row id of the warn to revoke (from check_ledger/user_history)")]
    pub ledger_id: i64,
    #[schemars(description = "who decided and why the warn comes off")]
    pub intent: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct RoleRequest {
    #[schemars(description = "user snowflake id")]
    pub user_id: String,
    #[schemars(description = "role snowflake id")]
    pub role_id: String,
    #[schemars(description = "who decided and why - recorded in the ledger")]
    pub intent: String,
    #[schemars(description = "reason shown in discord's own audit log")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct SetNicknameRequest {
    #[schemars(description = "user snowflake id whose nickname to change")]
    pub user_id: String,
    #[schemars(description = "the new nickname; empty string clears it back to the username")]
    pub nickname: String,
    #[schemars(description = "who decided and why - recorded in the ledger")]
    pub intent: String,
    #[schemars(description = "reason shown in discord's own audit log")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct DeleteMessageRequest {
    #[schemars(description = "channel snowflake id the message lives in")]
    pub channel_id: String,
    #[schemars(description = "message snowflake id to delete")]
    pub message_id: String,
    #[schemars(description = "who decided and why - recorded in the ledger")]
    pub intent: String,
    #[schemars(description = "reason shown in discord's own audit log")]
    pub reason: Option<String>,
}

#[tool_router(router = hands_router)]
impl KurouServer {
    #[tool(
        name = "ban_user",
        description = "Ban a user from the primary guild, with the calling sister's own bot. Requires intent; recorded in the mod ledger. Set dry_run to preview the target first."
    )]
    pub async fn ban_user(
        &self,
        Parameters(BanUserRequest { user_id, intent, reason, delete_message_days, duration_hours, dry_run }): Parameters<BanUserRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let target = parse_user(&user_id)?;
        let target_user = hand.client.user(target).await.map_err(tool_error)?;
        let days = delete_message_days.unwrap_or(0).min(7);
        let expires_at = duration_hours
            .filter(|hours| *hours > 0)
            .map(|hours| {
                Timestamp::from_unix_timestamp(Timestamp::now().unix_timestamp() + i64::from(hours) * 3600)
                    .map(|expiry| expiry.to_string())
                    .map_err(|error| format!("could not build tempban expiry: {error}"))
            })
            .transpose()?;
        if dry_run.unwrap_or(false) {
            return json_text(&serde_json::json!({
                "dry_run": true,
                "would_ban": { "id": target_user.id.to_string(), "username": target_user.name, "display": target_user.global_name },
                "delete_message_days": days,
                "expires_at": expires_at,
            }));
        }
        hand.client.ban(hand.guild, target, days, reason.as_deref()).await.map_err(tool_error)?;
        let mut row = hand.row("ban", intent).await;
        row.target_id = Some(target.to_string());
        row.target_name = Some(target_user.name.clone());
        row.reason = reason;
        row.expires_at = expires_at.clone();
        row.metadata = (days > 0 || duration_hours.is_some())
            .then(|| serde_json::json!({ "delete_message_days": days, "duration_hours": duration_hours }).to_string());
        let ledger_id = hand.record(row).await?;
        json_text(&serde_json::json!({ "banned": target_user.name, "ledger_id": ledger_id, "expires_at": expires_at }))
    }

    #[tool(
        name = "unban_user",
        description = "Lift a ban in the primary guild, with the calling sister's own bot. Requires intent; recorded in the mod ledger."
    )]
    pub async fn unban_user(
        &self,
        Parameters(UserActionRequest { user_id, intent, reason }): Parameters<UserActionRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let target = parse_user(&user_id)?;
        let target_user = hand.client.user(target).await.map_err(tool_error)?;
        // the open ban is looked up BEFORE the unban lands, so a re-ban racing in
        // after the REST call can never be the row this unban links to
        let open_ban = hand.modlog.latest_unreverted("ban", &target.to_string()).await.map_err(tool_error)?;
        hand.client.unban(hand.guild, target, reason.as_deref()).await.map_err(tool_error)?;
        let mut row = hand.row("unban", intent).await;
        row.target_id = Some(target.to_string());
        row.target_name = Some(target_user.name.clone());
        row.reason = reason;
        let ledger_id = match open_ban {
            Some(ban_id) => match hand.modlog.revert(ban_id, "ban", Source::Crow, row.clone()).await {
                Ok(id) => id,
                // the scheduler got there first - its reversal stands, ours records standalone
                Err(error) if error.to_string().contains("already reverted") => hand.record(row).await?,
                Err(error) => return Err(format!("the unban WENT THROUGH but the ledger write failed - record it by hand: {error:#}")),
            },
            None => hand.record(row).await?,
        };
        json_text(&serde_json::json!({ "unbanned": target_user.name, "ledger_id": ledger_id, "retired_ban": open_ban }))
    }

    #[tool(
        name = "kick_user",
        description = "Kick a member from the primary guild, with the calling sister's own bot. Requires intent; recorded in the mod ledger."
    )]
    pub async fn kick_user(
        &self,
        Parameters(UserActionRequest { user_id, intent, reason }): Parameters<UserActionRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let target = parse_user(&user_id)?;
        let target_user = hand.client.user(target).await.map_err(tool_error)?;
        hand.client.kick(hand.guild, target, reason.as_deref()).await.map_err(tool_error)?;
        let mut row = hand.row("kick", intent).await;
        row.target_id = Some(target.to_string());
        row.target_name = Some(target_user.name.clone());
        row.reason = reason;
        let ledger_id = hand.record(row).await?;
        json_text(&serde_json::json!({ "kicked": target_user.name, "ledger_id": ledger_id }))
    }

    #[tool(
        name = "timeout_user",
        description = "Time out a member (mute + no reactions) for a duration, with the calling sister's own bot. Discord expires it on its own. Requires intent; recorded in the mod ledger."
    )]
    pub async fn timeout_user(
        &self,
        Parameters(TimeoutUserRequest { user_id, duration_minutes, intent, reason }): Parameters<TimeoutUserRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let target = parse_user(&user_id)?;
        if duration_minutes == 0 {
            return Err(format!("duration_minutes must be 1-{MAX_TIMEOUT_MINUTES}; zero is not a timeout"));
        }
        let minutes = duration_minutes.min(MAX_TIMEOUT_MINUTES);
        let until = Timestamp::from_unix_timestamp(Timestamp::now().unix_timestamp() + i64::from(minutes) * 60)
            .map_err(|error| format!("could not build timeout timestamp: {error}"))?;
        let member = hand.client.timeout(hand.guild, target, until, reason.as_deref()).await.map_err(tool_error)?;
        let mut row = hand.row("timeout", intent).await;
        row.target_id = Some(target.to_string());
        row.target_name = Some(member.user.name.clone());
        row.reason = reason;
        row.metadata = Some(serde_json::json!({ "duration_minutes": minutes, "until": until.to_string() }).to_string());
        let ledger_id = hand.record(row).await?;
        json_text(&serde_json::json!({ "timed_out": member.user.name, "until": until.to_string(), "ledger_id": ledger_id }))
    }

    #[tool(
        name = "untimeout_user",
        description = "Lift a member's timeout early, with the calling sister's own bot. Requires intent; recorded in the mod ledger."
    )]
    pub async fn untimeout_user(
        &self,
        Parameters(UserActionRequest { user_id, intent, reason }): Parameters<UserActionRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let target = parse_user(&user_id)?;
        let open_timeout = hand.modlog.latest_unreverted("timeout", &target.to_string()).await.map_err(tool_error)?;
        let member = hand.client.untimeout(hand.guild, target, reason.as_deref()).await.map_err(tool_error)?;
        let mut row = hand.row("timeout_remove", intent).await;
        row.target_id = Some(target.to_string());
        row.target_name = Some(member.user.name.clone());
        row.reason = reason;
        let ledger_id = match open_timeout {
            Some(timeout_id) => match hand.modlog.revert(timeout_id, "timeout", Source::Crow, row.clone()).await {
                Ok(id) => id,
                Err(error) if error.to_string().contains("already reverted") => hand.record(row).await?,
                Err(error) => return Err(format!("the timeout lift WENT THROUGH but the ledger write failed - record it by hand: {error:#}")),
            },
            None => hand.record(row).await?,
        };
        json_text(&serde_json::json!({ "timeout_lifted": member.user.name, "ledger_id": ledger_id, "retired_timeout": open_timeout }))
    }

    #[tool(
        name = "warn_user",
        description = "Record a formal warning in the mod ledger. Discord has no native warn - the ledger row IS the warning; announce it in-channel yourself via send_message if it should be felt. Shows up in user_history and stacks toward escalation."
    )]
    pub async fn warn_user(
        &self,
        Parameters(WarnUserRequest { user_id, reason, intent }): Parameters<WarnUserRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        // warn's fingerprint check rides on reason - intent stays optional here because
        // the reason IS the warning text of record
        let hand = self.hand(&extensions, &reason)?;
        let target = parse_user(&user_id)?;
        let target_user = hand.client.user(target).await.map_err(tool_error)?;
        let intent = intent.filter(|intent| !intent.trim().is_empty()).unwrap_or_else(|| format!("{}: {}", hand.label, reason));
        let mut row = hand.row("warn", intent).await;
        row.target_id = Some(target.to_string());
        row.target_name = Some(target_user.name.clone());
        row.reason = Some(reason);
        let ledger_id = hand.record(row).await?;
        let strikes = hand.modlog.count_active("warn", &target.to_string()).await.map_err(tool_error)?;
        json_text(&serde_json::json!({ "warned": target_user.name, "ledger_id": ledger_id, "active_warns": strikes }))
    }

    #[tool(
        name = "revoke_warn",
        description = "Revoke a warning by its ledger row id. The original row is marked reverted and the revocation becomes its own ledger entry."
    )]
    pub async fn revoke_warn(
        &self,
        Parameters(RevokeWarnRequest { ledger_id, intent }): Parameters<RevokeWarnRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let row = hand.row("warn_revoke", intent).await;
        let reversal_id = hand.modlog.revert(ledger_id, "warn", Source::Crow, row).await.map_err(tool_error)?;
        json_text(&serde_json::json!({ "revoked": ledger_id, "reversal_id": reversal_id }))
    }

    #[tool(
        name = "add_role",
        description = "Give a member a role, with the calling sister's own bot. Requires intent; recorded in the mod ledger."
    )]
    pub async fn add_role(
        &self,
        Parameters(RoleRequest { user_id, role_id, intent, reason }): Parameters<RoleRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let target = parse_user(&user_id)?;
        let role = parse_role(&role_id)?;
        hand.client.add_role(hand.guild, target, role, reason.as_deref()).await.map_err(tool_error)?;
        let mut row = hand.row("role_add", intent).await;
        row.target_id = Some(target.to_string());
        row.reason = reason;
        row.metadata = Some(serde_json::json!({ "role_id": role.to_string() }).to_string());
        let ledger_id = hand.record(row).await?;
        json_text(&serde_json::json!({ "role_added": role.to_string(), "to": target.to_string(), "ledger_id": ledger_id }))
    }

    #[tool(
        name = "remove_role",
        description = "Take a role from a member, with the calling sister's own bot. Requires intent; recorded in the mod ledger."
    )]
    pub async fn remove_role(
        &self,
        Parameters(RoleRequest { user_id, role_id, intent, reason }): Parameters<RoleRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let target = parse_user(&user_id)?;
        let role = parse_role(&role_id)?;
        hand.client.remove_role(hand.guild, target, role, reason.as_deref()).await.map_err(tool_error)?;
        let mut row = hand.row("role_remove", intent).await;
        row.target_id = Some(target.to_string());
        row.reason = reason;
        row.metadata = Some(serde_json::json!({ "role_id": role.to_string() }).to_string());
        let ledger_id = hand.record(row).await?;
        json_text(&serde_json::json!({ "role_removed": role.to_string(), "from": target.to_string(), "ledger_id": ledger_id }))
    }

    #[tool(
        name = "set_nickname",
        description = "Change (or clear, with an empty string) a member's nickname, with the calling sister's own bot. Requires intent; recorded in the mod ledger."
    )]
    pub async fn set_nickname(
        &self,
        Parameters(SetNicknameRequest { user_id, nickname, intent, reason }): Parameters<SetNicknameRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let target = parse_user(&user_id)?;
        let previous = self.client.member(hand.guild, target).await.ok().and_then(|member| member.nick);
        let member = hand.client.set_nickname(hand.guild, target, &nickname, reason.as_deref()).await.map_err(tool_error)?;
        let mut row = hand.row("nickname", intent).await;
        row.target_id = Some(target.to_string());
        row.target_name = Some(member.user.name.clone());
        row.reason = reason;
        row.metadata = Some(serde_json::json!({ "nickname": nickname, "previous": previous }).to_string());
        let ledger_id = hand.record(row).await?;
        json_text(&serde_json::json!({ "renamed": member.user.name, "nickname": nickname, "ledger_id": ledger_id }))
    }

    #[tool(
        name = "delete_message",
        description = "Delete one message as a moderation act, with the calling sister's own bot. The content is snapshotted into the ledger before it dies. Requires intent."
    )]
    pub async fn delete_message(
        &self,
        Parameters(DeleteMessageRequest { channel_id, message_id, intent, reason }): Parameters<DeleteMessageRequest>,
        extensions: rmcp::model::Extensions,
    ) -> Result<String, String> {
        let hand = self.hand(&extensions, &intent)?;
        let channel = parse_channel(&channel_id)?;
        let message = parse_message(&message_id)?;
        self.guard_primary_channel(channel).await?;
        let snapshot = self.client.message(channel, message).await.map_err(tool_error)?;
        hand.client.delete_message(channel, message, reason.as_deref()).await.map_err(tool_error)?;
        let mut row = hand.row("delete", intent).await;
        row.target_id = Some(snapshot.author.id.to_string());
        row.target_name = Some(snapshot.author.name.clone());
        row.channel_id = Some(channel.to_string());
        row.reason = reason;
        row.metadata = Some(serde_json::json!({ "message_id": message.to_string(), "content": snapshot.content }).to_string());
        let ledger_id = hand.record(row).await?;
        json_text(&serde_json::json!({ "deleted": message.to_string(), "author": snapshot.author.name, "ledger_id": ledger_id }))
    }
}
