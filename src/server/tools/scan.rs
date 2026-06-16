use std::collections::HashSet;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use serenity::http::MessagePagination;
use serenity::model::channel::Message;

use crate::discord::types::messages_block;
use crate::server::KurouServer;
use crate::server::tools::common::{parse_channel, parse_message, tool_error};

const PAGE_SIZE: u8 = 100;
const DEFAULT_MAX_PAGES: u8 = 10;
const MAX_MAX_PAGES: u8 = 50;

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::scan_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct ScanChannelRequest {
    #[schemars(description = "channel (or thread) snowflake id to sweep")]
    pub channel_id: String,
    #[schemars(description = "keep only messages from these author ids (matched on id)")]
    pub author_ids: Option<Vec<String>>,
    #[schemars(description = "keep only messages mentioning any of these user ids")]
    pub mention_ids: Option<Vec<String>>,
    #[schemars(description = "keep only messages whose content contains this text (case-insensitive substring)")]
    pub text: Option<String>,
    #[schemars(description = "how many pages of 100 to sweep before stopping, 1-50, defaults to 10")]
    pub max_pages: Option<u8>,
    #[schemars(description = "start sweeping older than this message id (default: the latest message). pass an earlier oldest_scanned_id to continue a previous sweep")]
    pub before: Option<String>,
    #[schemars(description = "stop sweeping once messages are older than this message id (lower bound)")]
    pub after: Option<String>,
}

struct Filters {
    authors: HashSet<u64>,
    mentions: HashSet<u64>,
    text: Option<String>,
}

impl Filters {
    fn matches(&self, message: &Message) -> bool {
        if !self.authors.is_empty() && !self.authors.contains(&message.author.id.get()) {
            return false;
        }
        if !self.mentions.is_empty()
            && !message.mentions.iter().any(|u| self.mentions.contains(&u.id.get()))
        {
            return false;
        }
        if let Some(needle) = &self.text
            && !message.content.to_lowercase().contains(needle)
        {
            return false;
        }
        true
    }
}

#[tool_router(router = scan_router)]
impl KurouServer {
    #[tool(
        name = "scan_channel",
        description = "Deep-sweep a channel or thread by paging backward through history, filtering by author, mention, and/or text (case-insensitive substring). This is the heavy read - it makes several API calls, bounded by max_pages. Our own search, since Discord's search endpoint is closed to bots. Returns matched message blocks plus a meta line; if reached_cap is true there may be more history behind oldest_scanned_id - continue by passing it as `before`."
    )]
    pub async fn scan_channel(
        &self,
        Parameters(req): Parameters<ScanChannelRequest>,
    ) -> Result<String, String> {
        let channel = parse_channel(&req.channel_id)?;
        let filters = Filters {
            authors: parse_id_set(req.author_ids)?,
            mentions: parse_id_set(req.mention_ids)?,
            text: req.text.map(|t| t.to_lowercase()),
        };
        let max_pages = req.max_pages.unwrap_or(DEFAULT_MAX_PAGES).clamp(1, MAX_MAX_PAGES);
        let floor = req.after.as_deref().map(parse_message).transpose()?.map(|m| m.get());
        // resolve the bot once so a 50-page sweep doesn't re-probe every page
        let client = self.client_for_channel(channel).await;

        let mut cursor = req.before.as_deref().map(parse_message).transpose()?.map(|m| m.get());
        let mut matches: Vec<Message> = Vec::new();
        let mut scanned = 0usize;
        let mut pages = 0u8;
        let mut oldest: Option<u64> = None;
        let mut reached_cap = false;

        loop {
            let anchor = cursor.map(|id| MessagePagination::Before(serenity::model::id::MessageId::new(id)));
            let page = client.messages(channel, anchor, PAGE_SIZE).await.map_err(tool_error)?;
            if page.is_empty() {
                break; // ran out of channel
            }
            pages += 1;

            let mut hit_floor = false;
            for message in page {
                let id = message.id.get();
                if floor.is_some_and(|f| id <= f) {
                    hit_floor = true;
                    break;
                }
                scanned += 1;
                oldest = Some(id);
                cursor = Some(id);
                if filters.matches(&message) {
                    matches.push(message);
                }
            }

            if hit_floor {
                break; // reached the requested lower bound, this is a real end
            }
            if pages >= max_pages {
                reached_cap = true; // stopped on budget, there may be more behind oldest
                break;
            }
        }

        Ok(render(scanned, pages, reached_cap, oldest, &matches))
    }
}

fn render(scanned: usize, pages: u8, reached_cap: bool, oldest: Option<u64>, matches: &[Message]) -> String {
    let oldest = oldest.map(|id| id.to_string()).unwrap_or_else(|| "none".to_string());
    let meta = format!(
        "[scan] scanned={scanned} pages={pages} reached_cap={reached_cap} oldest_scanned_id={oldest} matches={}",
        matches.len()
    );
    if matches.is_empty() {
        format!("{meta}\n(no matches)")
    } else {
        format!("{meta}\n\n{}", messages_block(matches))
    }
}

fn parse_id_set(ids: Option<Vec<String>>) -> Result<HashSet<u64>, String> {
    ids.unwrap_or_default()
        .iter()
        .map(|raw| {
            raw.trim()
                .parse::<u64>()
                .map_err(|_| format!("'{raw}' is not a valid snowflake id"))
        })
        .collect()
}
