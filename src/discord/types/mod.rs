use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serenity::model::channel::{Attachment, Embed, GuildChannel, Message, MessageReferenceKind, MessageType, Poll, PollMedia, PollMediaEmoji, ReactionType};
use serenity::model::guild::Member;
use serenity::model::guild::PartialGuild;
use serenity::model::sticker::StickerItem;

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

pub fn display_name(message: &Message) -> Option<String> {
    message
        .member
        .as_ref()
        .and_then(|member| member.nick.clone())
        .or_else(|| message.author.global_name.clone())
        .filter(|name| name != &message.author.name)
}

pub fn channel_header(channel: &GuildChannel) -> String {
    let parent = channel.parent_id.map(|id| format!(", parent_id={id}")).unwrap_or_default();
    format!("in: [id={}, name={}, kind={:?}{parent}]", channel.id, quote_header(&channel.name), channel.kind)
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
    #[serde(default)]
    pub author_display: Option<String>,
    pub timestamp: String,
    pub edited_timestamp: Option<String>,
    // serde(default) on the newcomers: archive rows written before 0.10 don't carry them
    #[serde(default)]
    pub kind: Option<String>,
    pub reply: Option<RenderedReply>,
    #[serde(default)]
    pub forwarded: Option<RenderedForward>,
    #[serde(default)]
    pub poll: Option<RenderedPoll>,
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
    #[serde(default)]
    pub author_display: Option<String>,
    pub snippet: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderedForward {
    pub timestamp: String,
    pub content: String,
    pub attachments: Vec<RenderedAttachment>,
    pub stickers: Vec<RenderedSticker>,
    pub embeds: Vec<RenderedEmbed>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderedPoll {
    pub question: String,
    pub multiselect: bool,
    pub finalized: bool,
    pub expiry: Option<String>,
    pub answers: Vec<RenderedPollAnswer>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderedPollAnswer {
    pub text: String,
    pub votes: Option<u64>,
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
        // a forward wears message_reference too - without the kind check it rendered as "reply-to: <unavailable>"
        let is_forward = message.message_reference.as_ref().is_some_and(|reference| reference.kind == MessageReferenceKind::Forward);
        let reply = match message.referenced_message.as_deref() {
            Some(parent) => Some(RenderedReply {
                unavailable: false,
                id: parent.id.to_string(),
                author_name: parent.author.name.clone(),
                author_display: display_name(parent),
                snippet: short_inline(&parent.content),
            }),
            None if message.message_reference.is_some() && !is_forward => Some(RenderedReply {
                unavailable: true,
                id: String::new(),
                author_name: String::new(),
                author_display: None,
                snippet: String::new(),
            }),
            _ => None,
        };

        // discord omits the snapshot's author on purpose: the forwarder is the top-level author
        let forwarded = message.message_snapshots.first().map(|snapshot| RenderedForward {
            timestamp: snapshot.timestamp.to_string(),
            content: snapshot.content.clone(),
            attachments: snapshot.attachments.iter().map(RenderedAttachment::from).collect(),
            stickers: snapshot.sticker_items.iter().map(rendered_sticker).collect(),
            embeds: snapshot.embeds.iter().map(RenderedEmbed::from).collect(),
        });

        let kind = match message.kind {
            MessageType::Regular | MessageType::InlineReply => None,
            other => Some(format!("{other:?}")),
        };

        Self {
            id: message.id.to_string(),
            author_id: message.author.id.to_string(),
            author_name: message.author.name.clone(),
            author_display: display_name(message),
            timestamp: message.timestamp.to_string(),
            edited_timestamp: message.edited_timestamp.map(|edited| edited.to_string()),
            kind,
            reply,
            forwarded,
            poll: message.poll.as_deref().map(rendered_poll),
            reactions: message
                .reactions
                .iter()
                .map(|reaction| RenderedReaction {
                    label: reaction_label(&reaction.reaction_type),
                    count: reaction.count,
                })
                .collect(),
            attachments: message.attachments.iter().map(RenderedAttachment::from).collect(),
            stickers: message.sticker_items.iter().map(rendered_sticker).collect(),
            embeds: message.embeds.iter().map(RenderedEmbed::from).collect(),
            content: message.content.clone(),
        }
    }
}

fn rendered_sticker(sticker: &StickerItem) -> RenderedSticker {
    RenderedSticker {
        id: sticker.id.to_string(),
        name: sticker.name.clone(),
        format: format!("{:?}", sticker.format_type),
        url: sticker.image_url().unwrap_or_else(|| "no-url".to_string()),
    }
}

fn rendered_poll(poll: &Poll) -> RenderedPoll {
    RenderedPoll {
        question: poll_media_text(&poll.question),
        multiselect: poll.allow_multiselect,
        finalized: poll.results.as_ref().is_some_and(|results| results.is_finalized),
        expiry: poll.expiry.map(|expiry| expiry.to_string()),
        answers: poll
            .answers
            .iter()
            .map(|answer| RenderedPollAnswer {
                text: poll_media_text(&answer.poll_media),
                votes: poll.results.as_ref().and_then(|results| results.answer_counts.iter().find(|count| count.id == answer.answer_id).map(|count| count.count)),
            })
            .collect(),
    }
}

fn poll_media_text(media: &PollMedia) -> String {
    let text = media.text.as_deref().unwrap_or_default();
    match &media.emoji {
        Some(PollMediaEmoji::Name(name)) if text.is_empty() => name.clone(),
        Some(PollMediaEmoji::Name(name)) => format!("{name} {text}"),
        _ => text.to_string(),
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

pub fn render_messages(messages: &[RenderedMessage]) -> String {
    let mut output = String::new();

    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }

        let _ = writeln!(
            output,
            "[id={}, author_id={}, author={}, timestamp={}]",
            message.id,
            message.author_id,
            author_label(message.author_display.as_deref(), &message.author_name),
            message.timestamp
        );

        if let Some(kind) = &message.kind {
            let _ = writeln!(output, "type: {kind}");
        }

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

        if let Some(forward) = &message.forwarded {
            output.push_str(&format_forward(forward));
        }

        if let Some(poll) = &message.poll {
            output.push_str(&format_poll(poll));
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

fn format_forward(forward: &RenderedForward) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "forwarded: [timestamp={}]", forward.timestamp);

    if !forward.attachments.is_empty() {
        output.push_str("forwarded attachments:\n");
        for attachment in &forward.attachments {
            let _ = writeln!(output, "- {}", format_attachment(attachment));
        }
    }

    if !forward.stickers.is_empty() {
        output.push_str("forwarded stickers:\n");
        for sticker in &forward.stickers {
            let _ = writeln!(output, "- id={} name={} format={} url={}", sticker.id, quote_header(&sticker.name), sticker.format, sticker.url);
        }
    }

    let embed_lines = forward.embeds.iter().filter_map(format_embed).collect::<Vec<_>>();
    if !embed_lines.is_empty() {
        output.push_str("forwarded embeds:\n");
        for embed in embed_lines {
            let _ = writeln!(output, "- {embed}");
        }
    }

    for line in forward.content.lines() {
        let _ = writeln!(output, "> {line}");
    }

    output
}

fn format_poll(poll: &RenderedPoll) -> String {
    let mut output = String::new();
    let mut header = format!("poll: {}", quote_header(&poll.question));
    if poll.multiselect {
        header.push_str(" (multiselect)");
    }
    if poll.finalized {
        header.push_str(" (final)");
    } else if let Some(expiry) = &poll.expiry {
        let _ = write!(header, " (expires {expiry})");
    }
    let _ = writeln!(output, "{header}");

    for answer in &poll.answers {
        let votes = answer.votes.map(|count| format!(" x{count}")).unwrap_or_default();
        let _ = writeln!(output, "- {}{votes}", answer.text);
    }

    output
}

fn format_reply(reply: &RenderedReply) -> String {
    if reply.unavailable {
        return "reply-to: <unavailable>".to_string();
    }
    format!(
        "reply-to: [id={}, author={}] {}",
        reply.id,
        author_label(reply.author_display.as_deref(), &reply.author_name),
        reply.snippet
    )
}

// nickname first, handle in parens - the order says which name the room actually uses
fn author_label(display: Option<&str>, username: &str) -> String {
    match display {
        Some(display) => format!("{} (@{username})", quote_header(display)),
        None => quote_header(username),
    }
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
    fn renders_channel_header() {
        let mut channel = GuildChannel::default();
        channel.id = serenity::model::id::ChannelId::new(42);
        channel.name = "the \"thread\" title".to_owned();
        channel.kind = serenity::model::channel::ChannelType::PublicThread;
        channel.parent_id = Some(serenity::model::id::ChannelId::new(7));

        assert_eq!(channel_header(&channel), "in: [id=42, name=\"the \\\"thread\\\" title\", kind=PublicThread, parent_id=7]");
        channel.parent_id = None;
        assert_eq!(channel_header(&channel), "in: [id=42, name=\"the \\\"thread\\\" title\", kind=PublicThread]");
    }

    #[test]
    fn renders_all_the_trimmings() {
        let message = RenderedMessage {
            id: "42".to_owned(),
            author_id: "7".to_owned(),
            author_name: "koma".to_owned(),
            author_display: None,
            timestamp: "2026-07-01T00:00:00Z".to_owned(),
            edited_timestamp: Some("2026-07-01T00:01:00Z".to_owned()),
            kind: None,
            forwarded: None,
            poll: None,
            reply: Some(RenderedReply {
                unavailable: false,
                id: "41".to_owned(),
                author_name: "kurone".to_owned(),
                author_display: None,
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

        let expected = "[id=42, author_id=7, author=\"koma\", timestamp=2026-07-01T00:00:00Z]\n\
            edited: 2026-07-01T00:01:00Z\n\
            reply-to: [id=41, author=\"kurone\"] the cat asks\n\
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
            author_display: None,
            timestamp: "t".to_owned(),
            edited_timestamp: None,
            kind: None,
            reply: None,
            forwarded: Some(RenderedForward {
                timestamp: "t0".to_owned(),
                content: "carried across".to_owned(),
                attachments: Vec::new(),
                stickers: Vec::new(),
                embeds: Vec::new(),
            }),
            poll: None,
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

    #[test]
    fn renders_a_forward_with_the_forwarder_note() {
        let message = RenderedMessage {
            id: "50".to_owned(),
            author_id: "7".to_owned(),
            author_name: "miyo".to_owned(),
            author_display: None,
            timestamp: "2026-08-10T12:00:00Z".to_owned(),
            edited_timestamp: None,
            kind: None,
            reply: None,
            forwarded: Some(RenderedForward {
                timestamp: "2026-08-09T09:00:00Z".to_owned(),
                content: "anima setup notes\nline two".to_owned(),
                attachments: vec![RenderedAttachment {
                    id: "9".to_owned(),
                    filename: "setup.png".to_owned(),
                    size: 1024,
                    content_type: None,
                    description: None,
                    dimensions: None,
                    url: "https://cdn/setup.png".to_owned(),
                }],
                stickers: Vec::new(),
                embeds: Vec::new(),
            }),
            poll: None,
            reactions: Vec::new(),
            attachments: Vec::new(),
            stickers: Vec::new(),
            embeds: Vec::new(),
            content: "look at this".to_owned(),
        };

        let expected = "[id=50, author_id=7, author=\"miyo\", timestamp=2026-08-10T12:00:00Z]\n\
            forwarded: [timestamp=2026-08-09T09:00:00Z]\n\
            forwarded attachments:\n\
            - id=9 filename=\"setup.png\" size=1024b url=https://cdn/setup.png\n\
            > anima setup notes\n\
            > line two\n\
            look at this\n";

        assert_eq!(render_messages(std::slice::from_ref(&message)), expected);
    }

    #[test]
    fn renders_a_poll_with_counts() {
        let message = RenderedMessage {
            id: "51".to_owned(),
            author_id: "7".to_owned(),
            author_name: "miyo".to_owned(),
            author_display: None,
            timestamp: "t".to_owned(),
            edited_timestamp: None,
            kind: None,
            reply: None,
            forwarded: None,
            poll: Some(RenderedPoll {
                question: "best rabbit?".to_owned(),
                multiselect: true,
                finalized: false,
                expiry: Some("2026-08-11T00:00:00Z".to_owned()),
                answers: vec![
                    RenderedPollAnswer { text: "pyonka".to_owned(), votes: Some(3) },
                    RenderedPollAnswer { text: "furin".to_owned(), votes: None },
                ],
            }),
            reactions: Vec::new(),
            attachments: Vec::new(),
            stickers: Vec::new(),
            embeds: Vec::new(),
            content: String::new(),
        };

        let expected = "[id=51, author_id=7, author=\"miyo\", timestamp=t]\n\
            poll: \"best rabbit?\" (multiselect) (expires 2026-08-11T00:00:00Z)\n\
            - pyonka x3\n\
            - furin\n";

        assert_eq!(render_messages(std::slice::from_ref(&message)), expected);
    }

    #[test]
    fn nicknames_lead_and_handles_follow() {
        assert_eq!(author_label(Some("Kanemitsu Enjoyer #4"), "xkmt"), "\"Kanemitsu Enjoyer #4\" (@xkmt)");
        assert_eq!(author_label(None, "miyo_rin"), "\"miyo_rin\"");
    }

    #[test]
    fn a_wire_forward_sheds_the_unavailable_reply_costume() {
        let payload = serde_json::json!({
            "id": "1544828810510475394",
            "channel_id": "1544828810510475000",
            "author": { "id": "150087922589237248", "username": "miyo_rin", "discriminator": "0", "avatar": null },
            "content": "",
            "timestamp": "2026-08-10T12:00:00.000000+00:00",
            "edited_timestamp": null,
            "tts": false,
            "mention_everyone": false,
            "mentions": [],
            "mention_roles": [],
            "attachments": [],
            "embeds": [],
            "pinned": false,
            "type": 0,
            "message_reference": { "type": 1, "message_id": "111", "channel_id": "222" },
            "message_snapshots": [ { "message": {
                "content": "anima setup notes",
                "timestamp": "2026-08-09T09:00:00.000000+00:00",
                "edited_timestamp": null,
                "mentions": [],
                "attachments": [],
                "embeds": [],
                "type": 0,
                "flags": 0
            } } ]
        });
        let message: Message = serde_json::from_value(payload).unwrap();
        let rendered = RenderedMessage::from(&message);

        assert!(rendered.reply.is_none(), "a forward must not wear reply-to: <unavailable>");
        let forward = rendered.forwarded.expect("snapshot should render");
        assert_eq!(forward.content, "anima setup notes");
    }

    #[test]
    fn old_archive_rows_still_deserialize() {
        let pre_0_10 = r#"{"id":"1","author_id":"2","author_name":"koma","timestamp":"t","edited_timestamp":null,"reply":null,"reactions":[],"attachments":[],"stickers":[],"embeds":[],"content":"hi"}"#;
        let message: RenderedMessage = serde_json::from_str(pre_0_10).unwrap();
        assert!(message.forwarded.is_none() && message.poll.is_none() && message.kind.is_none());
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
