use std::fmt::Write as _;

use serde::Serialize;
use serenity::model::channel::{
    Attachment, Embed, GuildChannel, Message, MessageReaction, ReactionType,
};
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

pub fn messages_block(messages: &[Message]) -> String {
    let mut output = String::new();

    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }

        let _ = writeln!(
            output,
            "[id={}, author_id={}, author_name={}, timestamp={}]",
            message.id,
            message.author.id,
            quote_header(&message.author.name),
            message.timestamp
        );

        if let Some(edited) = message.edited_timestamp {
            let _ = writeln!(output, "edited: {edited}");
        }

        if let Some(reply) = format_reply(message) {
            let _ = writeln!(output, "{reply}");
        }

        if !message.reactions.is_empty() {
            let reactions = message
                .reactions
                .iter()
                .map(format_reaction)
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

        if !message.sticker_items.is_empty() {
            output.push_str("stickers:\n");
            for sticker in &message.sticker_items {
                let url = sticker.image_url().unwrap_or_else(|| "no-url".to_string());
                let _ = writeln!(
                    output,
                    "- id={} name={} format={:?} url={}",
                    sticker.id,
                    quote_header(&sticker.name),
                    sticker.format_type,
                    url
                );
            }
        }

        let embed_lines = message
            .embeds
            .iter()
            .filter_map(format_embed)
            .collect::<Vec<_>>();
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

fn format_reply(message: &Message) -> Option<String> {
    match message.referenced_message.as_deref() {
        Some(parent) => {
            let snippet = short_inline(&parent.content);
            Some(format!(
                "reply-to: [id={}, author_name={}] {}",
                parent.id,
                quote_header(&parent.author.name),
                snippet
            ))
        }
        // reference set but no parent payload = the message it replied to is gone
        None if message.message_reference.is_some() => Some("reply-to: <unavailable>".to_string()),
        None => None,
    }
}

fn quote_header(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn format_reaction(reaction: &MessageReaction) -> String {
    format!(
        "{} x{}",
        reaction_label(&reaction.reaction_type),
        reaction.count
    )
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

fn format_attachment(attachment: &Attachment) -> String {
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

    if let Some((width, height)) = attachment.dimensions() {
        parts.push(format!("dimensions={}x{}", width, height));
    }

    parts.push(format!("url={}", attachment.url));
    parts.join(" ")
}

fn format_embed(embed: &Embed) -> Option<String> {
    let mut parts = Vec::new();

    if let Some(kind) = &embed.kind {
        parts.push(format!("type={}", quote_header(kind)));
    }

    if let Some(title) = &embed.title {
        parts.push(format!("title={}", quote_header(title)));
    }

    if let Some(description) = &embed.description {
        parts.push(format!(
            "description={}",
            quote_header(&short_inline(description))
        ));
    }

    if let Some(url) = &embed.url {
        parts.push(format!("url={url}"));
    }

    if let Some(image) = &embed.image {
        parts.push(format!("image={}", image.url));
    }

    if let Some(thumbnail) = &embed.thumbnail {
        parts.push(format!("thumbnail={}", thumbnail.url));
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
