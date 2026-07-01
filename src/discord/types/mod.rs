use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serenity::model::channel::{Attachment, Embed, GuildChannel, Message, ReactionType};
use serenity::model::guild::Member;
use serenity::model::guild::PartialGuild;

#[derive(Serialize)]
pub struct ServerInfo {
    pub id: String,
    pub name: String,
    pub member_count: Option<u64>,
    pub description: Option<String>,
}

impl From<PartialGuild> for ServerInfo {
    fn from(g: PartialGuild) -> Self {
        Self {
            id: g.id.to_string(),
            name: g.name,
            member_count: g.approximate_member_count,
            description: g.description,
        }
    }
}

#[derive(Serialize)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub topic: Option<String>,
}

impl From<GuildChannel> for ChannelInfo {
    fn from(c: GuildChannel) -> Self {
        Self {
            id: c.id.to_string(),
            name: c.name,
            kind: format!("{:?}", c.kind),
            topic: c.topic,
        }
    }
}

#[derive(Serialize)]
pub struct MessageInfo {
    pub id: String,
    pub author_id: String,
    pub author_name: String,
    pub content: String,
    pub timestamp: String,
}

impl From<Message> for MessageInfo {
    fn from(m: Message) -> Self {
        Self {
            id: m.id.to_string(),
            author_id: m.author.id.to_string(),
            author_name: m.author.name,
            content: m.content,
            timestamp: m.timestamp.to_string(),
        }
    }
}

// the intermediate both a live Message and a stored archive row render through, so the
// crow's read blocks look identical whether they came off the wire or out of the ledger.
// it's serde-round-trippable: the gateway stores it as the archive's json payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderedMessage {
    pub id: String,
    pub author_id: String,
    pub author_name: String,
    pub timestamp: String,
    pub edited_timestamp: Option<String>,
    pub reply: Option<RenderedReply>,
    pub reactions: Vec<RenderedReaction>,
    pub attachments: Vec<RenderedAttachment>,
    pub stickers: Vec<RenderedSticker>,
    pub embeds: Vec<RenderedEmbed>,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderedReply {
    // reference set but the parent payload is gone = the replied-to message was deleted.
    pub unavailable: bool,
    pub id: String,
    pub author_name: String,
    pub snippet: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderedReaction {
    pub label: String,
    pub count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderedAttachment {
    pub id: String,
    pub filename: String,
    pub size: u32,
    pub content_type: Option<String>,
    pub description: Option<String>,
    pub dimensions: Option<(u32, u32)>,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderedSticker {
    pub id: String,
    pub name: String,
    pub format: String,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderedEmbed {
    pub kind: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub image: Option<String>,
    pub thumbnail: Option<String>,
}

impl From<&Message> for RenderedMessage {
    fn from(message: &Message) -> Self {
        let reply = match message.referenced_message.as_deref() {
            Some(parent) => Some(RenderedReply {
                unavailable: false,
                id: parent.id.to_string(),
                author_name: parent.author.name.clone(),
                snippet: short_inline(&parent.content),
            }),
            None if message.message_reference.is_some() => Some(RenderedReply {
                unavailable: true,
                id: String::new(),
                author_name: String::new(),
                snippet: String::new(),
            }),
            None => None,
        };

        Self {
            id: message.id.to_string(),
            author_id: message.author.id.to_string(),
            author_name: message.author.name.clone(),
            timestamp: message.timestamp.to_string(),
            edited_timestamp: message.edited_timestamp.map(|edited| edited.to_string()),
            reply,
            reactions: message
                .reactions
                .iter()
                .map(|reaction| RenderedReaction {
                    label: reaction_label(&reaction.reaction_type),
                    count: reaction.count,
                })
                .collect(),
            attachments: message.attachments.iter().map(RenderedAttachment::from).collect(),
            stickers: message
                .sticker_items
                .iter()
                .map(|sticker| RenderedSticker {
                    id: sticker.id.to_string(),
                    name: sticker.name.clone(),
                    format: format!("{:?}", sticker.format_type),
                    url: sticker.image_url().unwrap_or_else(|| "no-url".to_string()),
                })
                .collect(),
            embeds: message.embeds.iter().map(RenderedEmbed::from).collect(),
            content: message.content.clone(),
        }
    }
}

impl From<&Attachment> for RenderedAttachment {
    fn from(attachment: &Attachment) -> Self {
        Self {
            id: attachment.id.to_string(),
            filename: attachment.filename.clone(),
            size: attachment.size,
            content_type: attachment.content_type.clone(),
            description: attachment.description.clone(),
            dimensions: attachment.dimensions(),
            url: attachment.url.clone(),
        }
    }
}

impl From<&Embed> for RenderedEmbed {
    fn from(embed: &Embed) -> Self {
        Self {
            kind: embed.kind.clone(),
            title: embed.title.clone(),
            description: embed.description.as_deref().map(short_inline),
            url: embed.url.clone(),
            image: embed.image.as_ref().map(|image| image.url.clone()),
            thumbnail: embed.thumbnail.as_ref().map(|thumbnail| thumbnail.url.clone()),
        }
    }
}

// the REST callers still hand over live Messages; they map through the intermediate here.
pub fn messages_block(messages: &[Message]) -> String {
    let rendered = messages.iter().map(RenderedMessage::from).collect::<Vec<_>>();
    render_messages(&rendered)
}

pub fn render_messages(messages: &[RenderedMessage]) -> String {
    let mut output = String::new();

    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }

        let _ = writeln!(
            output,
            "[id={}, author_id={}, author_name={}, timestamp={}]",
            message.id,
            message.author_id,
            quote_header(&message.author_name),
            message.timestamp
        );

        if let Some(edited) = &message.edited_timestamp {
            let _ = writeln!(output, "edited: {edited}");
        }

        if let Some(reply) = &message.reply {
            let _ = writeln!(output, "{}", format_reply(reply));
        }

        if !message.reactions.is_empty() {
            let reactions = message
                .reactions
                .iter()
                .map(|reaction| format!("{} x{}", reaction.label, reaction.count))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(output, "reactions: {reactions}");
        }

        if !message.attachments.is_empty() {
            output.push_str("attachments:\n");
            for attachment in &message.attachments {
                let _ = writeln!(output, "- {}", format_attachment(attachment));
            }
        }

        if !message.stickers.is_empty() {
            output.push_str("stickers:\n");
            for sticker in &message.stickers {
                let _ = writeln!(
                    output,
                    "- id={} name={} format={} url={}",
                    sticker.id,
                    quote_header(&sticker.name),
                    sticker.format,
                    sticker.url
                );
            }
        }

        let embed_lines = message.embeds.iter().filter_map(format_embed).collect::<Vec<_>>();
        if !embed_lines.is_empty() {
            output.push_str("embeds:\n");
            for embed in embed_lines {
                let _ = writeln!(output, "- {embed}");
            }
        }

        if !message.content.is_empty() {
            output.push_str(&message.content);
            if !message.content.ends_with('\n') {
                output.push('\n');
            }
        }
    }

    output
}

fn format_reply(reply: &RenderedReply) -> String {
    if reply.unavailable {
        return "reply-to: <unavailable>".to_string();
    }
    format!(
        "reply-to: [id={}, author_name={}] {}",
        reply.id,
        quote_header(&reply.author_name),
        reply.snippet
    )
}

fn quote_header(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn reaction_label(reaction_type: &ReactionType) -> String {
    match reaction_type {
        ReactionType::Unicode(value) => value.clone(),
        ReactionType::Custom { animated, id, name } => {
            let name = name.as_deref().unwrap_or("emoji");
            if *animated {
                format!("<a:{name}:{id}>")
            } else {
                format!("<:{name}:{id}>")
            }
        }
        _ => format!("{reaction_type:?}"),
    }
}

fn format_attachment(attachment: &RenderedAttachment) -> String {
    let mut parts = vec![
        format!("id={}", attachment.id),
        format!("filename={}", quote_header(&attachment.filename)),
        format!("size={}b", attachment.size),
    ];

    if let Some(content_type) = &attachment.content_type {
        parts.push(format!("type={}", quote_header(content_type)));
    }

    if let Some(description) = &attachment.description {
        parts.push(format!("description={}", quote_header(description)));
    }

    if let Some((width, height)) = attachment.dimensions {
        parts.push(format!("dimensions={width}x{height}"));
    }

    parts.push(format!("url={}", attachment.url));
    parts.join(" ")
}

fn format_embed(embed: &RenderedEmbed) -> Option<String> {
    let mut parts = Vec::new();

    if let Some(kind) = &embed.kind {
        parts.push(format!("type={}", quote_header(kind)));
    }

    if let Some(title) = &embed.title {
        parts.push(format!("title={}", quote_header(title)));
    }

    if let Some(description) = &embed.description {
        parts.push(format!("description={}", quote_header(description)));
    }

    if let Some(url) = &embed.url {
        parts.push(format!("url={url}"));
    }

    if let Some(image) = &embed.image {
        parts.push(format!("image={image}"));
    }

    if let Some(thumbnail) = &embed.thumbnail {
        parts.push(format!("thumbnail={thumbnail}"));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn short_inline(value: &str) -> String {
    let mut cleaned = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() > 180 {
        cleaned = cleaned.chars().take(177).collect();
        cleaned.push_str("...");
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_all_the_trimmings() {
        let message = RenderedMessage {
            id: "42".to_owned(),
            author_id: "7".to_owned(),
            author_name: "koma".to_owned(),
            timestamp: "2026-07-01T00:00:00Z".to_owned(),
            edited_timestamp: Some("2026-07-01T00:01:00Z".to_owned()),
            reply: Some(RenderedReply {
                unavailable: false,
                id: "41".to_owned(),
                author_name: "kurone".to_owned(),
                snippet: "the cat asks".to_owned(),
            }),
            reactions: vec![RenderedReaction { label: "🐦".to_owned(), count: 3 }],
            attachments: vec![RenderedAttachment {
                id: "9".to_owned(),
                filename: "moon.png".to_owned(),
                size: 2048,
                content_type: Some("image/png".to_owned()),
                description: None,
                dimensions: Some((800, 600)),
                url: "https://cdn/moon.png".to_owned(),
            }],
            stickers: Vec::new(),
            embeds: vec![RenderedEmbed {
                kind: Some("link".to_owned()),
                title: Some("a title".to_owned()),
                description: None,
                url: Some("https://x".to_owned()),
                image: None,
                thumbnail: None,
            }],
            content: "look up".to_owned(),
        };

        let expected = "[id=42, author_id=7, author_name=\"koma\", timestamp=2026-07-01T00:00:00Z]\n\
            edited: 2026-07-01T00:01:00Z\n\
            reply-to: [id=41, author_name=\"kurone\"] the cat asks\n\
            reactions: 🐦 x3\n\
            attachments:\n\
            - id=9 filename=\"moon.png\" size=2048b type=\"image/png\" dimensions=800x600 url=https://cdn/moon.png\n\
            embeds:\n\
            - type=\"link\" title=\"a title\" url=https://x\n\
            look up\n";

        assert_eq!(render_messages(std::slice::from_ref(&message)), expected);
    }

    #[test]
    fn round_trips_through_json() {
        let message = RenderedMessage {
            id: "1".to_owned(),
            author_id: "2".to_owned(),
            author_name: "koma".to_owned(),
            timestamp: "t".to_owned(),
            edited_timestamp: None,
            reply: None,
            reactions: Vec::new(),
            attachments: Vec::new(),
            stickers: Vec::new(),
            embeds: Vec::new(),
            content: "hi".to_owned(),
        };
        let json = serde_json::to_string(&message).unwrap();
        let back: RenderedMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(render_messages(std::slice::from_ref(&message)), render_messages(std::slice::from_ref(&back)));
    }
}

#[derive(Serialize)]
pub struct UserLookupInfo {
    pub id: String,
    pub username: String,
    pub nickname: Option<String>,
    pub display_name: String,
    pub mention: String,
}

impl From<Member> for UserLookupInfo {
    fn from(m: Member) -> Self {
        let display_name = m.display_name().to_string();
        Self {
            id: m.user.id.to_string(),
            mention: format!("<@{}>", m.user.id),
            username: m.user.name,
            nickname: m.nick,
            display_name,
        }
    }
}
