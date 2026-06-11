use serde::Serialize;
use serenity::model::channel::{GuildChannel, Message};
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

#[derive(Serialize)]
pub struct UserLookupInfo {
    pub id: String,
    pub username: String,
    pub nickname: Option<String>,
}

impl From<Member> for UserLookupInfo {
    fn from(m: Member) -> Self {
        Self {
            id: m.user.id.to_string(),
            username: m.user.name,
            nickname: m.nick,
        }
    }
}
