use anyhow::{Context as _, Result};
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::model::channel::Message;
use serenity::model::event::MessageUpdateEvent;
use serenity::model::gateway::{GatewayIntents, Ready};
use serenity::model::id::{ChannelId, GuildId, MessageId, UserId};
use serenity::model::user::OnlineStatus;
use tokio::task::JoinHandle;

use crate::archive::{MessageStore, NewMessage};
use crate::config::GatewayMode;
use crate::discord::types::{RenderedMessage, display_name};
use crate::mentions::{MentionStore, NewMention};
use crate::modlog::{ModlogStore, NewModAction, Source};
use crate::wall::event::{WallFanout, enrich};

#[derive(Clone)]
pub struct GatewayConfig {
    pub mode: GatewayMode,
    pub default_guild: Option<GuildId>,
    pub mention_keywords: Vec<String>,
    pub mention_store: Option<MentionStore>,
    pub archive: Option<MessageStore>,
    pub modlog: Option<ModlogStore>,
    // the sisters' sender bots. a mod action by one of these was crow-issued: the hand
    // already wrote the richer row (with intent), so the watcher must not echo it.
    pub crow_bot_ids: Vec<UserId>,
    pub fanout: Option<WallFanout>,
    // the guilds this gateway owns for the wall and the archive. when both bots share a
    // guild they both see the message, so only its owner records it - else we double up.
    pub broadcast_guilds: Vec<GuildId>,
}

pub fn spawn_gateway(token: String, config: GatewayConfig) -> Option<JoinHandle<()>> {
    if config.mode == GatewayMode::Off
        && config.fanout.is_none()
        && config.archive.is_none()
        && config.modlog.is_none()
    {
        return None;
    }

    Some(tokio::spawn(async move {
        if let Err(error) = run_gateway(&token, config).await {
            // {:#} or the chain stays hidden - a bare %error cost us four days once
            tracing::error!(error = format!("{error:#}"), "discord gateway stopped");
        }
    }))
}

async fn run_gateway(token: &str, config: GatewayConfig) -> Result<()> {
    let bot_user_id = serenity::http::Http::new(token)
        .get_current_user()
        .await
        .context("failed to fetch current bot user before gateway start")?
        .id;
    let mut intents = match config.mode {
        GatewayMode::Off => GatewayIntents::empty(),
        GatewayMode::Presence => GatewayIntents::GUILDS,
        GatewayMode::Mentions => {
            GatewayIntents::GUILDS
                | GatewayIntents::GUILD_MESSAGES
                | GatewayIntents::MESSAGE_CONTENT
        }
    };
    // the wall and the archive both need to hear every message, so either forces the
    // message intents on even when mention-recording is off.
    if config.fanout.is_some() || config.archive.is_some() {
        intents |= GatewayIntents::GUILDS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;
    }
    // GUILD_MEMBERS is privileged - it's granted in the dev portal (Mother's word,
    // 2026-09-06); if it ever gets revoked the whole gateway fails to identify.
    if config.modlog.is_some() {
        intents |= GatewayIntents::GUILDS | GatewayIntents::GUILD_MODERATION | GatewayIntents::GUILD_MEMBERS;
    }

    let handler = Handler {
        mode: config.mode,
        bot_user_id,
        default_guild: config.default_guild,
        mention_keywords: normalize_keywords(config.mention_keywords),
        mention_store: config.mention_store,
        archive: config.archive,
        modlog: config.modlog,
        crow_bot_ids: config.crow_bot_ids,
        seen_audit_entries: std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
        fanout: config.fanout,
        broadcast_guilds: config.broadcast_guilds,
    };
    let mut client = Client::builder(token, intents)
        .event_handler(handler)
        .await
        .context("failed to create discord gateway client")?;

    tracing::info!(mode = ?config.mode, "discord gateway starting");
    client
        .start()
        .await
        .context("discord gateway client failed")
}

#[derive(Clone)]
struct Handler {
    mode: GatewayMode,
    bot_user_id: UserId,
    default_guild: Option<GuildId>,
    mention_keywords: Vec<String>,
    mention_store: Option<MentionStore>,
    archive: Option<MessageStore>,
    modlog: Option<ModlogStore>,
    crow_bot_ids: Vec<UserId>,
    // audit entries already turned into rows. member-update events re-surface old
    // Update entries (a role change fires the event but writes a different audit
    // action), so freshness alone can't stop a timeout recording twice.
    seen_audit_entries: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<u64>>>,
    fanout: Option<WallFanout>,
    broadcast_guilds: Vec<GuildId>,
}

// who pulled the trigger. the ban event itself never says - the answer lives in the
// audit log, which can lag the gateway event by a beat, hence the patient retries.
struct Attribution {
    executor_id: Option<String>,
    executor_name: Option<String>,
    reason: Option<String>,
    entry: Option<serenity::model::guild::audit_log::AuditLogEntry>,
}

// finds the freshest audit entry for a target, or nothing - "nothing" is a real answer
// (a plain leave has no kick entry), so this one never complains about it.
async fn find_attribution(
    ctx: &Context,
    guild_id: GuildId,
    action: serenity::model::guild::audit_log::Action,
    target: UserId,
    delays_ms: &[u64],
    max_age_secs: i64,
) -> Option<Attribution> {
    for delay_ms in delays_ms {
        tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
        let logs = match guild_id.audit_logs(&ctx.http, Some(action), None, None, Some(10)).await {
            Ok(logs) => logs,
            Err(error) => {
                // a transient failure spends one attempt, not the whole hunt
                tracing::warn!(error = format!("{error:#}"), %guild_id, "audit log fetch failed");
                continue;
            }
        };
        // stale entries must not wear a new event's face - an old ban of the same user
        // is not this ban
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|now| now.as_secs() as i64)
            .unwrap_or(i64::MAX);
        let entry = logs.entries.into_iter().find(|entry| {
            entry.target_id.is_some_and(|id| id.get() == target.get())
                && now - entry.id.created_at().unix_timestamp() < max_age_secs
        });
        if let Some(entry) = entry {
            return Some(Attribution {
                executor_id: Some(entry.user_id.to_string()),
                executor_name: ctx.http.get_user(entry.user_id).await.ok().map(|user| user.name),
                reason: entry.reason.clone(),
                entry: Some(entry),
            });
        }
    }
    None
}

async fn attribute(
    ctx: &Context,
    guild_id: GuildId,
    action: serenity::model::guild::audit_log::Action,
    target: UserId,
) -> Attribution {
    match find_attribution(ctx, guild_id, action, target, &[500, 1500, 3000], 300).await {
        Some(attribution) => attribution,
        None => {
            tracing::warn!(%guild_id, %target, "no matching audit entry after retries; recording unattributed");
            Attribution { executor_id: None, executor_name: None, reason: None, entry: None }
        }
    }
}

impl Attribution {
    fn nobody() -> Self {
        Self { executor_id: None, executor_name: None, reason: None, entry: None }
    }
}

impl Handler {
    async fn record_observed(&self, action: &str, guild_id: GuildId, target: &serenity::model::user::User, attribution: Attribution, metadata: Option<String>) {
        let Some(modlog) = &self.modlog else {
            return;
        };
        let row = NewModAction {
            action: action.to_string(),
            guild_id: guild_id.to_string(),
            target_id: Some(target.id.to_string()),
            target_name: Some(target.name.clone()),
            executor_id: attribution.executor_id,
            executor_name: attribution.executor_name,
            reason: attribution.reason,
            metadata,
            ..Default::default()
        };
        match modlog.record(Source::Observed, row).await {
            Ok(_) => tracing::info!(action, target = %target.id, %guild_id, "recorded observed mod action"),
            Err(error) => tracing::error!(error = format!("{error:#}"), action, target = %target.id, "failed to record observed mod action"),
        }
    }

    fn owns_moderation(&self, guild_id: GuildId) -> bool {
        self.modlog.is_some() && self.default_guild == Some(guild_id)
    }

    fn already_recorded(&self, entry_id: u64) -> bool {
        let mut seen = self.seen_audit_entries.lock().expect("seen-entries lock poisoned");
        if seen.contains(&entry_id) {
            return true;
        }
        seen.push_back(entry_id);
        if seen.len() > 128 {
            seen.pop_front();
        }
        false
    }

    fn is_crow_executor(&self, attribution: &Attribution) -> bool {
        attribution
            .executor_id
            .as_deref()
            .and_then(|id| id.parse::<u64>().ok())
            .map(UserId::new)
            .is_some_and(|id| id == self.bot_user_id || self.crow_bot_ids.contains(&id))
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        ctx.set_presence(None, OnlineStatus::Online);
        tracing::info!(
            user = %ready.user.name,
            user_id = %ready.user.id,
            mode = ?self.mode,
            "discord gateway ready"
        );
    }

    async fn guild_ban_addition(&self, ctx: Context, guild_id: GuildId, banned_user: serenity::model::user::User) {
        if !self.owns_moderation(guild_id) {
            return;
        }
        let action = serenity::model::guild::audit_log::Action::Member(serenity::model::guild::audit_log::MemberAction::BanAdd);
        let attribution = attribute(&ctx, guild_id, action, banned_user.id).await;
        if self.is_crow_executor(&attribution) {
            tracing::debug!(target = %banned_user.id, "skipping observed ban: crow-issued, the hand already wrote it");
            return;
        }
        self.record_observed("ban", guild_id, &banned_user, attribution, None).await;
    }

    async fn guild_ban_removal(&self, ctx: Context, guild_id: GuildId, unbanned_user: serenity::model::user::User) {
        if !self.owns_moderation(guild_id) {
            return;
        }
        let action = serenity::model::guild::audit_log::Action::Member(serenity::model::guild::audit_log::MemberAction::BanRemove);
        let attribution = attribute(&ctx, guild_id, action, unbanned_user.id).await;
        if self.is_crow_executor(&attribution) {
            tracing::debug!(target = %unbanned_user.id, "skipping observed unban: crow-issued, the hand already wrote it");
            return;
        }
        self.record_observed("unban", guild_id, &unbanned_user, attribution, None).await;
    }

    async fn guild_member_addition(&self, _ctx: Context, new_member: serenity::model::guild::Member) {
        let guild_id = new_member.guild_id;
        if !self.owns_moderation(guild_id) {
            return;
        }
        let metadata = serde_json::json!({ "account_created": new_member.user.id.created_at().to_string() }).to_string();
        self.record_observed("join", guild_id, &new_member.user, Attribution::nobody(), Some(metadata)).await;
    }

    async fn guild_member_removal(&self, ctx: Context, guild_id: GuildId, user: serenity::model::user::User, _member_data: Option<serenity::model::guild::Member>) {
        if !self.owns_moderation(guild_id) {
            return;
        }
        use serenity::model::guild::audit_log::{Action, MemberAction};
        // the remove event can't tell a kick from a walk-out - only the audit log can
        if let Some(attribution) = find_attribution(&ctx, guild_id, Action::Member(MemberAction::Kick), user.id, &[1000, 2500], 30).await {
            if self.is_crow_executor(&attribution) {
                tracing::debug!(target = %user.id, "skipping observed kick: crow-issued, the hand already wrote it");
                return;
            }
            self.record_observed("kick", guild_id, &user, attribution, None).await;
            return;
        }
        // a ban fires this event too, and the ban handler owns that story
        if find_attribution(&ctx, guild_id, Action::Member(MemberAction::BanAdd), user.id, &[100], 30).await.is_some() {
            return;
        }
        self.record_observed("leave", guild_id, &user, Attribution::nobody(), None).await;
    }

    async fn guild_member_update(&self, ctx: Context, _old: Option<serenity::model::guild::Member>, _new: Option<serenity::model::guild::Member>, event: serenity::model::event::GuildMemberUpdateEvent) {
        if !self.owns_moderation(event.guild_id) {
            return;
        }
        use serenity::model::guild::audit_log::{Action, Change, MemberAction};
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|now| now.as_secs() as i64)
            .unwrap_or(i64::MAX);
        // member updates fire for nicks and avatars too; a fresh audit entry that touches
        // communication_disabled_until is what makes this one a timeout
        let timed_out = event.communication_disabled_until.is_some_and(|until| until.unix_timestamp() > now);
        let delays: &[u64] = if timed_out { &[500, 1500] } else { &[500] };
        let Some(attribution) = find_attribution(&ctx, event.guild_id, Action::Member(MemberAction::Update), event.user.id, delays, 30).await else {
            return;
        };
        let new_until = attribution.entry.as_ref().and_then(|entry| {
            entry.changes.as_ref()?.iter().find_map(|change| match change {
                Change::CommunicationDisabledUntil { new, .. } => Some(new.as_ref().cloned()),
                _ => None,
            })
        });
        let Some(new_until) = new_until else {
            return;
        };
        if self.is_crow_executor(&attribution) {
            tracing::debug!(target = %event.user.id, "skipping observed timeout change: crow-issued, the hand already wrote it");
            return;
        }
        if attribution.entry.as_ref().is_some_and(|entry| self.already_recorded(entry.id.get())) {
            return;
        }
        match new_until {
            Some(until) if until.unix_timestamp() > now => {
                let metadata = serde_json::json!({ "until": until.to_string() }).to_string();
                self.record_observed("timeout", event.guild_id, &event.user, attribution, Some(metadata)).await;
            }
            _ => self.record_observed("timeout_remove", event.guild_id, &event.user, attribution, None).await,
        }
    }

    async fn message_delete(&self, _ctx: Context, channel_id: ChannelId, deleted_message_id: MessageId, _guild_id: Option<GuildId>) {
        let Some(archive) = &self.archive else {
            return;
        };
        match archive.delete(deleted_message_id.get()).await {
            Ok(true) => {}
            Ok(false) => tracing::debug!(message_id = %deleted_message_id, channel_id = %channel_id, "ignored delete for unknown archive message"),
            Err(error) => tracing::error!(error = format!("{error:#}"), message_id = %deleted_message_id, channel_id = %channel_id, "failed to mark archived message deleted"),
        }
    }

    async fn message_delete_bulk(&self, _ctx: Context, channel_id: ChannelId, multiple_deleted_messages_ids: Vec<MessageId>, _guild_id: Option<GuildId>) {
        let Some(archive) = &self.archive else {
            return;
        };
        let requested = multiple_deleted_messages_ids.len();
        let message_ids = multiple_deleted_messages_ids.into_iter().map(MessageId::get).collect();
        match archive.delete_bulk(message_ids).await {
            Ok(changed) if changed == requested => {}
            Ok(changed) => tracing::debug!(changed, requested, channel_id = %channel_id, "bulk delete included unknown archive messages"),
            Err(error) => tracing::error!(error = format!("{error:#}"), channel_id = %channel_id, "failed to mark archived messages deleted"),
        }
    }

    async fn message_update(&self, _ctx: Context, _old_if_available: Option<Message>, _new: Option<Message>, event: MessageUpdateEvent) {
        let Some(archive) = &self.archive else {
            return;
        };
        let message_id = event.id;
        let channel_id = event.channel_id;
        let edited_timestamp = event.edited_timestamp.map(|timestamp| timestamp.to_string());
        match archive.edit(message_id.get(), event.content, edited_timestamp).await {
            Ok(true) => {}
            Ok(false) => tracing::debug!(message_id = %message_id, channel_id = %channel_id, "ignored archive message update without changed content"),
            Err(error) => tracing::error!(error = format!("{error:#}"), message_id = %message_id, channel_id = %channel_id, "failed to archive message edit"),
        }
    }

    async fn message(&self, _ctx: Context, message: Message) {
        // the wall wants everything, koma's own posts included. it never acts on a
        // message, so there's no echo loop to fear here - just a mirror. but only for the
        // guilds this gateway owns: if both bots are in a guild, the other one carries it.
        if let Some(fanout) = &self.fanout
            && message
                .guild_id
                .is_some_and(|guild| self.broadcast_guilds.contains(&guild))
        {
            let enriched = enrich(&fanout.client, &fanout.cache, &message).await;
            let _ = fanout.tx.send(std::sync::Arc::new(enriched));
        }

        // the archive keeps everything the crow owns, koma's own posts included - it's a
        // record of the room, not a mention filter. dedup rides the same guild-ownership gate.
        if let Some(archive) = &self.archive
            && message
                .guild_id
                .is_some_and(|guild| self.broadcast_guilds.contains(&guild))
        {
            let record = NewMessage {
                rendered: RenderedMessage::from(&message),
                guild_id: message.guild_id.map(|id| id.to_string()),
                channel_id: message.channel_id.to_string(),
                mention_ids: message.mentions.iter().map(|user| user.id.to_string()).collect(),
            };
            if let Err(error) = archive.insert(record).await {
                tracing::error!(error = format!("{error:#}"), message_id = %message.id, channel_id = %message.channel_id, "failed to archive message");
            }
        }

        if self.mode != GatewayMode::Mentions {
            return;
        }
        if message.author.id == self.bot_user_id {
            return;
        }
        if self
            .default_guild
            .is_some_and(|guild| message.guild_id != Some(guild))
        {
            return;
        }

        let Some(store) = &self.mention_store else {
            tracing::warn!("mention gateway mode is enabled without a mention store");
            return;
        };
        let matched = matched_terms(&message, self.bot_user_id, &self.mention_keywords);
        if matched.is_empty() {
            return;
        }

        let mention = NewMention {
            message_id: message.id.to_string(),
            guild_id: message.guild_id.map(|id| id.to_string()),
            channel_id: message.channel_id.to_string(),
            author_id: message.author.id.to_string(),
            author_name: message.author.name.clone(),
            author_display_name: display_name(&message),
            content: message.content.clone(),
            matched: matched.join(","),
            timestamp: message.timestamp.to_string(),
            link: message.id.link(message.channel_id, message.guild_id),
        };

        match store.insert(mention).await {
            Ok(true) => tracing::info!(
                message_id = %message.id,
                channel_id = %message.channel_id,
                author_id = %message.author.id,
                "stored mention"
            ),
            Ok(false) => {}
            Err(error) => tracing::error!(error = format!("{error:#}"), "failed to store mention"),
        }
    }
}

fn normalize_keywords(keywords: Vec<String>) -> Vec<String> {
    keywords
        .into_iter()
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| !keyword.is_empty())
        .collect()
}

fn matched_terms(message: &Message, bot_user_id: UserId, keywords: &[String]) -> Vec<String> {
    let mut matched = Vec::new();
    if message.mentions.iter().any(|user| user.id == bot_user_id) {
        matched.push("mention".to_string());
    }

    let content = message.content.to_lowercase();
    for keyword in keywords {
        if content.contains(keyword) && !matched.iter().any(|item| item == keyword) {
            matched.push(keyword.clone());
        }
    }

    matched
}

