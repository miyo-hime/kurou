pub mod event;

use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use serenity::model::channel::ChannelType;
use serenity::model::id::{ChannelId, GuildId};
use tokio::sync::broadcast;
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};

use crate::discord::DiscordClient;
use crate::layout::LayoutStore;
use crate::wall::event::{EnrichCache, WallMessage, enrich};

const PAGE: &str = include_str!("nightwatch.html");

const BACKFILL_DEFAULT: u8 = 30;
const BACKFILL_MAX: u8 = 100;

// which token can read which guild. the wall only ever reads, so this is the soft
// read-routing twin of send.rs's hard gate, not the gate itself.
#[derive(Clone)]
pub struct ClientPool {
    pub client: DiscordClient,
    pub readonly_client: Option<DiscordClient>,
    pub default_guild: Option<GuildId>,
    pub readonly_guilds: Vec<GuildId>,
}

impl ClientPool {
    fn client_for_guild(&self, guild: GuildId) -> &DiscordClient {
        match &self.readonly_client {
            Some(observer) if Some(guild) != self.default_guild => observer,
            _ => &self.client,
        }
    }

    async fn client_for_channel(&self, channel: ChannelId) -> &DiscordClient {
        let Some(observer) = &self.readonly_client else {
            return &self.client;
        };
        match self.client.channel_guild(channel).await {
            Ok(guild) if guild == self.default_guild => &self.client,
            _ => observer,
        }
    }

    fn guilds(&self) -> Vec<(GuildId, &'static str)> {
        let mut out = Vec::new();
        if let Some(primary) = self.default_guild {
            out.push((primary, "primary"));
        }
        out.extend(self.readonly_guilds.iter().map(|g| (*g, "observer")));
        out
    }
}

#[derive(Clone)]
pub struct WallState {
    pub tx: broadcast::Sender<Arc<WallMessage>>,
    pub cache: Arc<EnrichCache>,
    pub pool: ClientPool,
    pub layout: LayoutStore,
}

pub fn router(state: WallState) -> Router {
    Router::new()
        .route("/wall", get(page))
        .route("/wall/events", get(events))
        .route("/wall/backfill", get(backfill))
        .route("/wall/sources", get(sources))
        .route("/wall/layout", get(get_layout).put(put_layout))
        .with_state(state)
}

async fn page() -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], PAGE).into_response()
}

// the firehose. every message the gateways see lands here; the browser keeps only the
// channels it has a panel for and drops the rest on the floor. audience of one, behind
// authelia, on localhost - filtering server-side would be paying for a problem we don't have.
async fn events(
    State(state): State<WallState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(state.tx.subscribe()).filter_map(|res| {
        let message = res.ok()?;
        let event = Event::default().json_data(&*message).ok()?;
        Some(Ok(event))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct BackfillParams {
    channel_id: String,
    limit: Option<u8>,
}

async fn backfill(State(state): State<WallState>, Query(params): Query<BackfillParams>) -> Response {
    let Ok(channel) = params.channel_id.trim().parse::<u64>().map(ChannelId::new) else {
        return error(StatusCode::BAD_REQUEST, "channel_id must be a snowflake");
    };
    let limit = params.limit.unwrap_or(BACKFILL_DEFAULT).clamp(1, BACKFILL_MAX);

    let client = state.pool.client_for_channel(channel).await;
    let messages = match client.messages(channel, None, limit).await {
        Ok(messages) => messages,
        Err(error) => {
            tracing::warn!(%error, %channel, "backfill read failed");
            return self::error(StatusCode::BAD_GATEWAY, "could not read that channel");
        }
    };

    // discord hands these newest-first; the panel appends top-down, so flip to chronological.
    let mut out = Vec::with_capacity(messages.len());
    for message in messages.iter().rev() {
        out.push(enrich(client, &state.cache, message).await);
    }
    Json(out).into_response()
}

#[derive(Serialize)]
struct WallSource {
    id: String,
    name: String,
    kind: &'static str,
    channels: Vec<WallChannel>,
}

#[derive(Serialize)]
struct WallChannel {
    id: String,
    name: String,
    kind: &'static str,
}

async fn sources(State(state): State<WallState>) -> Response {
    let mut out = Vec::new();
    for (guild, kind) in state.pool.guilds() {
        let client = state.pool.client_for_guild(guild);
        let name = client
            .server_info(guild)
            .await
            .map(|info| info.name)
            .unwrap_or_else(|_| guild.to_string());

        let mut channels = Vec::new();
        if let Ok(guild_channels) = client.channels(guild).await {
            let mut text: Vec<_> = guild_channels
                .into_iter()
                .filter(|c| matches!(c.kind, ChannelType::Text | ChannelType::News))
                .collect();
            text.sort_by_key(|c| c.position);
            channels.extend(text.into_iter().map(|c| WallChannel {
                id: c.id.to_string(),
                name: c.name,
                kind: "channel",
            }));
        }
        if let Ok(threads) = client.active_threads(guild).await {
            channels.extend(threads.into_iter().map(|t| WallChannel {
                id: t.id.to_string(),
                name: t.name,
                kind: "thread",
            }));
        }

        out.push(WallSource {
            id: guild.to_string(),
            name,
            kind,
            channels,
        });
    }
    Json(out).into_response()
}

async fn get_layout(State(state): State<WallState>) -> Response {
    match state.layout.get().await {
        Ok(Some(stored)) => (
            [(header::CONTENT_TYPE, "application/json")],
            stored,
        )
            .into_response(),
        Ok(None) => Json(json!({ "panels": [] })).into_response(),
        Err(error) => {
            tracing::error!(%error, "layout read failed");
            self::error(StatusCode::INTERNAL_SERVER_ERROR, "could not read the layout")
        }
    }
}

async fn put_layout(State(state): State<WallState>, body: String) -> Response {
    if serde_json::from_str::<serde_json::Value>(&body).is_err() {
        return error(StatusCode::BAD_REQUEST, "layout body must be json");
    }
    match state.layout.put(body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error, "layout write failed");
            self::error(StatusCode::INTERNAL_SERVER_ERROR, "could not save the layout")
        }
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}
