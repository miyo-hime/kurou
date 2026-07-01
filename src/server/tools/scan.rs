use std::collections::HashSet;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use serenity::model::channel::Message;
use serenity::model::id::ChannelId;
use serenity::http::MessagePagination;

use crate::archive::ScanQuery;
use crate::discord::types::{messages_block, render_messages};
use crate::server::KurouServer;
use crate::server::tools::common::{parse_channel, parse_message, tool_error};

const PAGE_SIZE: u8 = 100;
const DEFAULT_MAX_PAGES: u8 = 10;
const MAX_MAX_PAGES: u8 = 50;
const DEFAULT_ARCHIVE_LIMIT: u16 = 100;
const MAX_ARCHIVE_LIMIT: u16 = 500;

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
    #[schemars(description = "where to read: 'auto' (archive if ARCHIVE is on, else REST), 'archive' (local db only, fast), 'rest' (page Discord, full history). defaults to auto")]
    pub source: Option<String>,
    #[schemars(description = "REST only: how many pages of 100 to sweep before stopping, 1-50, defaults to 10")]
    pub max_pages: Option<u8>,
    #[schemars(description = "archive only: max matches to return, 1-500, defaults to 100")]
    pub limit: Option<u16>,
    #[schemars(description = "start sweeping older than this message id (default: the latest message). pass an earlier oldest_scanned_id to continue a previous sweep")]
    pub before: Option<String>,
    #[schemars(description = "stop sweeping once messages are older than this message id (lower bound)")]
    pub after: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Auto,
    Archive,
    Rest,
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
        description = "Deep-sweep a channel or thread for messages by author, mention, and/or text (case-insensitive substring). Discord's search endpoint is closed to bots, so this is ours. With ARCHIVE on, source=auto answers from the local archive instantly (bounded by its coverage floor); source=rest pages Discord for full history (heavy, several API calls bounded by max_pages). Returns matched message blocks plus a meta line."
    )]
    pub async fn scan_channel(
        &self,
        Parameters(req): Parameters<ScanChannelRequest>,
    ) -> Result<String, String> {
        let channel = parse_channel(&req.channel_id)?;
        let source = parse_source(req.source.as_deref())?;
        if source == Source::Archive && self.message_store.is_none() {
            return Err("message archive is disabled; set ARCHIVE=true or use source=rest".to_string());
        }
        let use_archive = match source {
            Source::Archive => true,
            Source::Rest => false,
            Source::Auto => self.message_store.is_some(),
        };

        if use_archive {
            self.scan_via_archive(channel, req).await
        } else {
            self.scan_via_rest(channel, req).await
        }
    }
}

impl KurouServer {
    async fn scan_via_archive(
        &self,
        channel: ChannelId,
        req: ScanChannelRequest,
    ) -> Result<String, String> {
        let store = self
            .message_store
            .as_ref()
            .ok_or_else(|| "message archive is disabled; set ARCHIVE=true".to_string())?;

        let query = ScanQuery {
            channel_id: channel.get().to_string(),
            author_ids: canonical_ids(req.author_ids)?,
            mention_ids: canonical_ids(req.mention_ids)?,
            text: req.text,
            before: req.before.as_deref().map(parse_message).transpose()?.map(|m| m.get() as i64),
            after: req.after.as_deref().map(parse_message).transpose()?.map(|m| m.get() as i64),
            limit: req.limit.unwrap_or(DEFAULT_ARCHIVE_LIMIT).clamp(1, MAX_ARCHIVE_LIMIT),
        };

        let scan = store.scan(query).await.map_err(tool_error)?;
        let floor = scan.floor.map(|id| id.to_string()).unwrap_or_else(|| "none".to_string());
        let meta = format!(
            "[scan] source=archive matches={} archive_floor={floor} (older than the floor is only in source=rest)",
            scan.matches.len()
        );
        if scan.matches.is_empty() {
            Ok(format!("{meta}\n(no matches)"))
        } else {
            Ok(format!("{meta}\n\n{}", render_messages(&scan.matches)))
        }
    }

    async fn scan_via_rest(
        &self,
        channel: ChannelId,
        req: ScanChannelRequest,
    ) -> Result<String, String> {
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

        Ok(render_rest(scanned, pages, reached_cap, oldest, &matches))
    }
}

fn parse_source(raw: Option<&str>) -> Result<Source, String> {
    match raw.map(str::trim).map(str::to_lowercase).as_deref() {
        None | Some("") | Some("auto") => Ok(Source::Auto),
        Some("archive") => Ok(Source::Archive),
        Some("rest") => Ok(Source::Rest),
        Some(other) => Err(format!("source must be auto, archive, or rest (got '{other}')")),
    }
}

fn render_rest(scanned: usize, pages: u8, reached_cap: bool, oldest: Option<u64>, matches: &[Message]) -> String {
    let oldest = oldest.map(|id| id.to_string()).unwrap_or_else(|| "none".to_string());
    let meta = format!(
        "[scan] source=rest scanned={scanned} pages={pages} reached_cap={reached_cap} oldest_scanned_id={oldest} matches={}",
        matches.len()
    );
    if matches.is_empty() {
        format!("{meta}\n(no matches)")
    } else {
        format!("{meta}\n\n{}", messages_block(matches))
    }
}

// validate each id is a snowflake and hand back its canonical decimal string (what the
// archive stored), so a stray space or leading zero can't silently miss.
fn canonical_ids(ids: Option<Vec<String>>) -> Result<Vec<String>, String> {
    parse_id_set(ids).map(|set| set.into_iter().map(|id| id.to_string()).collect())
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
