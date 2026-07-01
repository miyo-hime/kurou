use anyhow::{Context as _, Result};
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::model::channel::Message;
use serenity::model::gateway::{GatewayIntents, Ready};
use serenity::model::id::{GuildId, UserId};
use serenity::model::user::OnlineStatus;
use tokio::task::JoinHandle;

use crate::archive::{MessageStore, NewMessage};
use crate::config::GatewayMode;
use crate::discord::types::RenderedMessage;
use crate::mentions::{MentionStore, NewMention};
use crate::wall::event::{WallFanout, enrich};

#[derive(Clone)]
pub struct GatewayConfig {
    pub mode: GatewayMode,
    pub default_guild: Option<GuildId>,
    pub mention_keywords: Vec<String>,
    pub mention_store: Option<MentionStore>,
    pub archive: Option<MessageStore>,
    pub fanout: Option<WallFanout>,
    // the guilds this gateway owns for the wall and the archive. when both bots share a
    // guild they both see the message, so only its owner records it - else we double up.
    pub broadcast_guilds: Vec<GuildId>,
}

pub fn spawn_gateway(token: String, config: GatewayConfig) -> Option<JoinHandle<()>> {
    if config.mode == GatewayMode::Off && config.fanout.is_none() && config.archive.is_none() {
        return None;
    }

    Some(tokio::spawn(async move {
        if let Err(error) = run_gateway(&token, config).await {
            tracing::error!(%error, "discord gateway stopped");
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

    let handler = Handler {
        mode: config.mode,
        bot_user_id,
        default_guild: config.default_guild,
        mention_keywords: normalize_keywords(config.mention_keywords),
        mention_store: config.mention_store,
        archive: config.archive,
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
    fanout: Option<WallFanout>,
    broadcast_guilds: Vec<GuildId>,
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
                tracing::error!(%error, message_id = %message.id, "failed to archive message");
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
            Err(error) => tracing::error!(%error, "failed to store mention"),
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

fn display_name(message: &Message) -> Option<String> {
    message.member.as_ref().and_then(|member| {
        member
            .nick
            .clone()
            .or_else(|| message.author.global_name.clone())
            .filter(|name| name != &message.author.name)
    })
}
