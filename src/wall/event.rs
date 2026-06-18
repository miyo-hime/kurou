use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serenity::model::channel::{Attachment, Message, MessageReaction, ReactionType};
use serenity::model::id::{GuildId, RoleId, UserId};
use serenity::model::sticker::{StickerFormatType, StickerItem};
use tokio::sync::broadcast;

use crate::discord::DiscordClient;

// roles drift slowly, members slower from the crow's view. these are just to keep a
// busy channel from hammering the REST role/member endpoints on every single message.
const ROLE_TTL: Duration = Duration::from_secs(600);
const MEMBER_TTL: Duration = Duration::from_secs(300);

// long edge of an attachment thumbnail. media.discordapp.net resizes server-side, so
// the browser pulls a cheap preview instead of the full art.
const THUMB_EDGE: u32 = 400;

// one enriched block. the live gateway path and the REST backfill path both produce
// this exact shape, so the browser renders one thing and never has to care which door
// a message came through.
#[derive(Clone, Debug, Serialize)]
pub struct WallMessage {
    pub id: String,
    pub channel_id: String,
    pub guild_id: Option<String>,
    pub author: WallAuthor,
    pub timestamp: String,
    pub edited: bool,
    pub content: String,
    pub reply: Option<WallReply>,
    pub reactions: Vec<WallReaction>,
    pub attachments: Vec<WallAttachment>,
    pub stickers: Vec<WallSticker>,
    // <@id> -> display name, so the browser can swap user mentions to readable handles
    pub mentions: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WallAuthor {
    pub id: String,
    pub name: String,
    pub username: String,
    pub color: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WallReply {
    pub author: String,
    pub snippet: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WallReaction {
    pub emoji: Option<String>,
    pub custom: Option<WallEmoji>,
    pub count: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct WallEmoji {
    pub id: String,
    pub name: String,
    pub animated: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct WallAttachment {
    pub kind: String,
    pub name: String,
    pub size: u64,
    pub url: String,
    pub thumb: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WallSticker {
    pub name: String,
    pub url: Option<String>,
}

// the live side of the bird. a gateway hands each message here; the broadcast carries
// it to whatever browsers happen to be watching. nobody listening = nobody hurt.
#[derive(Clone)]
pub struct WallFanout {
    pub client: DiscordClient,
    pub cache: Arc<EnrichCache>,
    pub tx: broadcast::Sender<Arc<WallMessage>>,
}

#[derive(Clone, Copy)]
struct RoleColor {
    position: i64,
    colour: u32,
}

struct CachedRoles {
    colored: Arc<HashMap<RoleId, RoleColor>>,
    at: Instant,
}

struct CachedMember {
    roles: Vec<RoleId>,
    nick: Option<String>,
    at: Instant,
}

#[derive(Default)]
pub struct EnrichCache {
    roles: Mutex<HashMap<GuildId, CachedRoles>>,
    members: Mutex<HashMap<(GuildId, UserId), CachedMember>>,
}

impl EnrichCache {
    pub fn new() -> Self {
        Self::default()
    }

    async fn colored_roles(
        &self,
        client: &DiscordClient,
        guild: GuildId,
    ) -> Arc<HashMap<RoleId, RoleColor>> {
        if let Some(fresh) = self
            .roles
            .lock()
            .expect("role cache poisoned")
            .get(&guild)
            .filter(|c| c.at.elapsed() < ROLE_TTL)
            .map(|c| c.colored.clone())
        {
            return fresh;
        }

        let colored = match client.guild_roles(guild).await {
            Ok(roles) => roles
                .into_iter()
                .filter(|r| r.colour.0 != 0)
                .map(|r| {
                    (
                        r.id,
                        RoleColor {
                            position: i64::from(r.position),
                            colour: r.colour.0,
                        },
                    )
                })
                .collect(),
            Err(error) => {
                tracing::debug!(%error, %guild, "role fetch failed, no color this round");
                HashMap::new()
            }
        };

        let colored = Arc::new(colored);
        self.roles.lock().expect("role cache poisoned").insert(
            guild,
            CachedRoles {
                colored: colored.clone(),
                at: Instant::now(),
            },
        );
        colored
    }

    // backfill messages rarely carry the member partial, so we go find the roles/nick
    // ourselves. cached because a quiet channel is the same five people all night.
    async fn member(
        &self,
        client: &DiscordClient,
        guild: GuildId,
        user: UserId,
    ) -> Option<(Vec<RoleId>, Option<String>)> {
        if let Some(hit) = self
            .members
            .lock()
            .expect("member cache poisoned")
            .get(&(guild, user))
            .filter(|c| c.at.elapsed() < MEMBER_TTL)
            .map(|c| (c.roles.clone(), c.nick.clone()))
        {
            return Some(hit);
        }

        let member = client.member(guild, user).await.ok()?;
        let roles = member.roles.clone();
        let nick = member.nick.clone();
        self.members.lock().expect("member cache poisoned").insert(
            (guild, user),
            CachedMember {
                roles: roles.clone(),
                nick: nick.clone(),
                at: Instant::now(),
            },
        );
        Some((roles, nick))
    }

    async fn color_for(
        &self,
        client: &DiscordClient,
        guild: GuildId,
        roles: &[RoleId],
    ) -> Option<String> {
        if roles.is_empty() {
            return None;
        }
        let colored = self.colored_roles(client, guild).await;
        roles
            .iter()
            .filter_map(|id| colored.get(id))
            .max_by_key(|rc| rc.position)
            .map(|rc| format!("#{:06x}", rc.colour))
    }
}

pub async fn enrich(client: &DiscordClient, cache: &EnrichCache, message: &Message) -> WallMessage {
    let guild = message.guild_id;

    // gateway gives us the member partial for free; REST backfill makes us ask.
    let (roles, nick) = match (&message.member, guild) {
        (Some(partial), _) => (partial.roles.clone(), partial.nick.clone()),
        (None, Some(g)) => cache
            .member(client, g, message.author.id)
            .await
            .unwrap_or_default(),
        (None, None) => (Vec::new(), None),
    };

    let color = match guild {
        Some(g) => cache.color_for(client, g, &roles).await,
        None => None,
    };

    let name = nick
        .or_else(|| message.author.global_name.clone())
        .unwrap_or_else(|| message.author.name.clone());

    let mentions = message
        .mentions
        .iter()
        .map(|user| {
            let shown = user
                .global_name
                .clone()
                .unwrap_or_else(|| user.name.clone());
            (user.id.to_string(), shown)
        })
        .collect();

    WallMessage {
        id: message.id.to_string(),
        channel_id: message.channel_id.to_string(),
        guild_id: guild.map(|g| g.to_string()),
        author: WallAuthor {
            id: message.author.id.to_string(),
            name,
            username: message.author.name.clone(),
            color,
        },
        timestamp: message.timestamp.to_string(),
        edited: message.edited_timestamp.is_some(),
        content: message.content.clone(),
        reply: reply(message),
        reactions: message.reactions.iter().map(reaction).collect(),
        attachments: message.attachments.iter().map(attachment).collect(),
        stickers: message.sticker_items.iter().map(sticker).collect(),
        mentions,
    }
}

fn reply(message: &Message) -> Option<WallReply> {
    match message.referenced_message.as_deref() {
        Some(parent) => {
            let author = parent
                .author
                .global_name
                .clone()
                .unwrap_or_else(|| parent.author.name.clone());
            Some(WallReply {
                author,
                snippet: short_inline(&parent.content),
            })
        }
        None if message.message_reference.is_some() => Some(WallReply {
            author: String::new(),
            snippet: "<unavailable>".to_string(),
        }),
        None => None,
    }
}

fn reaction(reaction: &MessageReaction) -> WallReaction {
    match &reaction.reaction_type {
        ReactionType::Unicode(value) => WallReaction {
            emoji: Some(value.clone()),
            custom: None,
            count: reaction.count,
        },
        ReactionType::Custom { animated, id, name } => WallReaction {
            emoji: None,
            custom: Some(WallEmoji {
                id: id.to_string(),
                name: name.clone().unwrap_or_else(|| "emoji".to_string()),
                animated: *animated,
            }),
            count: reaction.count,
        },
        other => WallReaction {
            emoji: Some(format!("{other:?}")),
            custom: None,
            count: reaction.count,
        },
    }
}

fn attachment(attachment: &Attachment) -> WallAttachment {
    let content_type = attachment.content_type.as_deref().unwrap_or("");
    let (kind, is_image) = if content_type.starts_with("image/") {
        ("image", true)
    } else if content_type.starts_with("video/") {
        ("video", false)
    } else {
        ("file", false)
    };

    let dims = attachment.dimensions();
    let thumb = is_image.then(|| sized_thumb(&attachment.proxy_url, dims));

    WallAttachment {
        kind: kind.to_string(),
        name: attachment.filename.clone(),
        size: u64::from(attachment.size),
        url: attachment.url.clone(),
        thumb,
        width: dims.map(|(w, _)| w),
        height: dims.map(|(_, h)| h),
    }
}

// scale the long edge down to THUMB_EDGE, keep aspect. unknown dims just gets a width cap.
fn sized_thumb(proxy_url: &str, dims: Option<(u32, u32)>) -> String {
    let (w, h) = match dims {
        Some((w, h)) if w > 0 && h > 0 => {
            if w >= h {
                (THUMB_EDGE.min(w), (THUMB_EDGE.min(w) * h / w).max(1))
            } else {
                ((THUMB_EDGE.min(h) * w / h).max(1), THUMB_EDGE.min(h))
            }
        }
        _ => (THUMB_EDGE, THUMB_EDGE),
    };
    let sep = if proxy_url.contains('?') { '&' } else { '?' };
    format!("{proxy_url}{sep}width={w}&height={h}")
}

fn sticker(sticker: &StickerItem) -> WallSticker {
    let url = match sticker.format_type {
        StickerFormatType::Lottie => None,
        _ => sticker.image_url(),
    };
    WallSticker {
        name: sticker.name.clone(),
        url,
    }
}

fn short_inline(value: &str) -> String {
    let mut cleaned = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() > 120 {
        cleaned = cleaned.chars().take(117).collect();
        cleaned.push_str("...");
    }
    cleaned
}
