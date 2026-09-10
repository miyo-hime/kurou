mod tools;

use std::collections::HashMap;
use std::sync::Arc;

use std::time::Duration;

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    tool_handler,
    transport::{
        stdio,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use serenity::model::id::{GuildId, UserId};
use tokio_util::sync::CancellationToken;

use crate::archive::MessageStore;
use crate::auth::AuthConfig;
use crate::config::{Config, GatewayMode};
use crate::discord::DiscordClient;
use crate::gateway::GatewayConfig;
use crate::ledger::Ledger;
use crate::mentions::MentionStore;
use crate::modlog::ModlogStore;
use crate::uploads::UploadStore;
use crate::wall::event::{EnrichCache, WallFanout, WallMessage};
use crate::wall::{ClientPool, WallState};

// the observer bot and the guilds it owns, bundled because they share a boot-time
// invariant: secondaries exist iff the observer does.
#[derive(Clone, Debug)]
pub(crate) struct Observer {
    pub(crate) client: DiscordClient,
    pub(crate) guilds: Vec<GuildId>,
}

// primary + secondaries + readonly, parsed and cross-checked once at boot.
#[derive(Clone, Debug, Default)]
pub(crate) struct GuildTopology {
    pub(crate) primary: Option<GuildId>,
    pub(crate) secondary: Vec<GuildId>,
    pub(crate) readonly: Vec<GuildId>,
}

#[derive(Clone, Debug)]
pub struct KurouServer {
    pub(crate) client: DiscordClient,
    pub(crate) observer: Option<Observer>,
    pub(crate) default_guild: Option<GuildId>,
    pub(crate) secondary_guilds: Vec<GuildId>,
    pub(crate) mention_store: Option<MentionStore>,
    pub(crate) message_store: Option<MessageStore>,
    pub(crate) modlog_store: Option<ModlogStore>,
    pub(crate) upload_store: UploadStore,
    pub(crate) senders: Arc<HashMap<String, DiscordClient>>,
    pub(crate) wake_dm_from: Vec<UserId>,
    pub(crate) presence: Option<crate::gateway::PresenceSlot>,
    tool_router: ToolRouter<Self>,
}

impl KurouServer {
    // ※ at clippy's arity ceiling - next field bundles the ledger tenants
    // (mention/message/modlog stores) into one struct
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        client: DiscordClient,
        observer: Option<Observer>,
        default_guild: Option<GuildId>,
        mention_store: Option<MentionStore>,
        message_store: Option<MessageStore>,
        modlog_store: Option<ModlogStore>,
        upload_store: UploadStore,
        senders: Arc<HashMap<String, DiscordClient>>,
    ) -> Self {
        Self {
            client,
            observer,
            default_guild,
            secondary_guilds: Vec::new(),
            mention_store,
            message_store,
            modlog_store,
            upload_store,
            senders,
            wake_dm_from: Vec::new(),
            presence: None,
            tool_router: Self::tool_router(),
        }
    }

    pub fn with_wake_dm_from(mut self, ids: Vec<UserId>) -> Self {
        self.wake_dm_from = ids;
        self
    }

    pub(crate) fn with_secondary_guilds(mut self, guilds: Vec<GuildId>) -> Self {
        self.secondary_guilds = guilds;
        self
    }

    pub fn with_presence(mut self, slot: Option<crate::gateway::PresenceSlot>) -> Self {
        self.presence = slot;
        self
    }

    pub(crate) fn readonly_guilds(&self) -> &[GuildId] {
        self.observer.as_ref().map(|observer| observer.guilds.as_slice()).unwrap_or(&[])
    }

    pub(crate) fn is_writable(&self, guild: GuildId) -> bool {
        self.default_guild == Some(guild) || self.secondary_guilds.contains(&guild)
    }

    // the mouth belongs to whoever's asking: koma speaks with the primary bot, a sister
    // speaks with her own - and without one she has eyes here, not a voice.
    pub(crate) fn sender_for(&self, identity: &str) -> Result<&DiscordClient, String> {
        if identity == "koma" {
            return Ok(&self.client);
        }
        self.senders.get(identity).ok_or_else(|| {
            format!(
                "'{identity}' is read-only on the crow: no DISCORD_TOKEN_{} is configured, and the crow won't speak as koma on someone else's behalf",
                identity.to_uppercase()
            )
        })
    }

    // guild is known: primary token for the writable guilds, observer for a readonly.
    pub(crate) fn client_for_guild(&self, guild: GuildId) -> &DiscordClient {
        match &self.observer {
            Some(observer) if !self.is_writable(guild) => &observer.client,
            _ => &self.client,
        }
    }

    // only a channel id in hand. with no observer it's always primary; otherwise probe
    // once - the primary bot sees its writable guilds' channels and 403s on the readonly.
    pub(crate) async fn client_for_channel(
        &self,
        channel: serenity::model::id::ChannelId,
    ) -> &DiscordClient {
        let Some(observer) = &self.observer else {
            return &self.client;
        };
        match self.client.channel_guild(channel).await {
            Ok(Some(guild)) if self.is_writable(guild) => &self.client,
            // a guildless channel is the primary bot's own DM - the observer never met it
            Ok(None) => &self.client,
            _ => &observer.client,
        }
    }

    fn tool_router() -> ToolRouter<Self> {
        ToolRouter::new()
            + tools::info::router()
            + tools::channels::router()
            + tools::messages::router()
            + tools::scan::router()
            + tools::mentions::router()
            + tools::archive::router()
            + tools::send::router()
            + tools::typing::router()
            + tools::reaction::router()
            + tools::users::router()
            + tools::modlog::router()
            + tools::presence::router()
            + tools::hands::router()
            + tools::raid::router()
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "kurou",
    // the macro refuses env!, so this drifts from Cargo.toml unless bumped by hand
    version = "0.23.0",
    instructions = "a small window into discord servers. crow on the wire. guilds wear one of three hats: the primary (the home guild and default reach - when asked to check a message or channel with no server named, look here first), writable secondaries (the crow's bots live and speak there too), and readonly guilds (watch-only, a separate observer bot, routed for you). list_servers tells you which is which. reads: list_servers, get_server_info, list_channels (the guild's custom emoji and sticker names ride along - they're the server's culture, reach for them when they fit), list_threads, read_messages (anchor with around/before/after), get_message, get_pinned, scan_channel (deep author/mention/text sweep). archive: search_messages (full-text search the local message archive, needs ARCHIVE=true). voice (primary + secondary guilds only): send_message (guild stickers may ride along via sticker_ids), typing (raise the indicator as your own bot, one shot), add_reaction / remove_reaction (an emoji on a message in your own bot's voice - unicode as-is, custom as <:name:id>), set_presence (steer the primary bot's dot - perch-driven, primary voice only), get_user_id_by_name. mentions: check_mentions, mark_mentions_seen. mod ledger: check_ledger, user_history - the crow's moderation memory, every action it witnessed or performed. mod hands (primary guild only, caller's own bot, intent required, every act recorded): ban_user, unban_user, kick_user, timeout_user, untimeout_user, warn_user, revoke_warn, add_role, remove_role, set_nickname, delete_message, lock_channel, unlock_channel, set_slowmode, purge_channel, delete_invite; get_bans and list_invites are open reads. the watcher also records what other moderators do: bans, unbans, kicks, timeouts, joins and leaves land as observed ledger rows. the private wire: DM channels of WAKE_DM_FROM users may be read and answered; every other DM does not exist to the crow. multi-identity: every caller is a labeled bearer - reads are open to all sisters, send_message and the mod hands act with the caller's own bot voice or refuse, and the mention inbox answers only to koma."
)]
impl ServerHandler for KurouServer {}

// uploads are meant to be claimed seconds later by send_message. ten minutes is
// generous slack, not a parking lot.
const UPLOAD_TTL: Duration = Duration::from_secs(600);

pub async fn run_stdio(config: Config) -> Result<()> {
    let token = config
        .discord_token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
        .context("DISCORD_TOKEN is required (no token, no window)")?
        .to_string();
    let topology = parse_topology(&config)?;
    let default_guild = topology.primary;
    let observer = build_observer(&config, &topology)?;

    // stdio is the local smoke path - the wall and the archive are http-only, so the
    // ledger only opens for the mention inbox or the mod layer.
    let ledger = if config.gateway_mode == GatewayMode::Mentions || config.modlog {
        let ledger = Ledger::open(&config.ledger_path()).await?;
        tracing::info!(path = %config.ledger_path().display(), "ledger opened");
        Some(ledger)
    } else {
        None
    };
    let mention_store = (config.gateway_mode == GatewayMode::Mentions)
        .then(|| ledger.as_ref().map(Ledger::mentions))
        .flatten();
    let notifier = build_modlog_notifier(&config, &token)?;
    let modlog_store = config.modlog
        .then(|| ledger.as_ref().map(Ledger::modlog))
        .flatten()
        .map(|store| store.with_notifier(notifier));

    let presence_slot = config.wake_url.is_some().then(crate::gateway::PresenceSlot::default);
    let gateway = crate::gateway::spawn_gateway(
        token.clone(),
        GatewayConfig {
            mode: config.gateway_mode,
            default_guild,
            secondary_guilds: topology.secondary.clone(),
            mention_keywords: config.mention_keywords.clone(),
            mention_store: mention_store.clone(),
            archive: None,
            modlog: modlog_store.clone(),
            crow_bot_ids: Vec::new(),
            fanout: None,
            broadcast_guilds: Vec::new(),
            wake: crate::wake::WakeSender::from_config(config.wake_url.as_deref(), config.wake_secret.as_deref()),
            wake_dm_from: parse_wake_dm_from(&config.wake_dm_from),
            presence: presence_slot.clone(),
        },
    );

    let client = DiscordClient::new(&token);
    // stdio sessions are short-lived, but a ticking scheduler still beats a tempban
    // nobody is timing; the http daemon drains anything this one misses.
    let scheduler = modlog_store
        .clone()
        .map(|modlog| crate::scheduler::spawn_scheduler(client.clone(), modlog));
    let upload_store = UploadStore::new(UPLOAD_TTL);
    let service = KurouServer::new(
        client,
        observer,
        default_guild,
        mention_store,
        None,
        modlog_store,
        upload_store,
        Arc::default(),
    )
    .with_secondary_guilds(topology.secondary.clone())
    .with_wake_dm_from(parse_wake_dm_from(&config.wake_dm_from))
    .with_presence(presence_slot)
    .serve(stdio())
    .await?;
    tracing::info!("kurou running on stdio");
    service.waiting().await?;
    if let Some(gateway) = gateway {
        gateway.abort();
    }
    if let Some(scheduler) = scheduler {
        scheduler.abort();
    }
    Ok(())
}

pub async fn run_http(config: Config) -> Result<()> {
    let token = config
        .discord_token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
        .context("DISCORD_TOKEN is required (no token, no window)")?
        .to_string();
    let topology = parse_topology(&config)?;
    let default_guild = topology.primary;
    let observer = build_observer(&config, &topology)?;
    let bind_addr: std::net::SocketAddr = (config.host, config.port).into();

    // one ledger for the whole crow. it opens when any tenant is live: the mention inbox,
    // the wall's saved layout, or the archive. each store draws off the shared handle.
    let needs_ledger =
        config.wall || config.archive || config.modlog || config.gateway_mode == GatewayMode::Mentions;
    let ledger = if needs_ledger {
        let ledger = Ledger::open(&config.ledger_path()).await?;
        tracing::info!(path = %config.ledger_path().display(), "ledger opened");
        Some(ledger)
    } else {
        None
    };
    let mention_store = (config.gateway_mode == GatewayMode::Mentions)
        .then(|| ledger.as_ref().map(Ledger::mentions))
        .flatten();
    let archive_store = config
        .archive
        .then(|| ledger.as_ref().map(Ledger::archive))
        .flatten();
    let notifier = build_modlog_notifier(&config, &token)?;
    let modlog_store = config.modlog
        .then(|| ledger.as_ref().map(Ledger::modlog))
        .flatten()
        .map(|store| store.with_notifier(notifier));
    if modlog_store.as_ref().is_some_and(|_| config.modlog_channel.is_some()) {
        tracing::info!(channel = ?config.modlog_channel, "modlog embeds posting");
    }

    // the wall's plumbing: one enrichment cache shared by every gateway and the backfill
    // path, one broadcast both gateways pour into and every browser drinks from.
    let wall_enabled = config.wall;
    let enrich_cache = Arc::new(EnrichCache::new());
    let (wall_tx, _wall_rx) = tokio::sync::broadcast::channel::<Arc<WallMessage>>(256);

    let primary_fanout = wall_enabled.then(|| WallFanout {
        client: DiscordClient::new(&token),
        cache: enrich_cache.clone(),
        tx: wall_tx.clone(),
    });

    let senders: Arc<HashMap<String, DiscordClient>> = Arc::new(
        crate::config::sender_tokens()
            .into_iter()
            .map(|(label, sender_token)| (label, DiscordClient::new(&sender_token)))
            .collect(),
    );
    if !senders.is_empty() {
        tracing::info!(voices = ?senders.keys().collect::<Vec<_>>(), "per-sister bot voices configured");
    }
    // the watcher needs to know the crow's own hands by their bot ids, or a crow-issued
    // ban would land twice: once with intent, once as a hollow observed echo.
    let mut crow_bot_ids = Vec::new();
    for (label, sender) in senders.iter() {
        match sender.current_user_id().await {
            Ok(id) => crow_bot_ids.push(id),
            Err(error) => tracing::warn!(label, error = format!("{error:#}"), "could not resolve sender bot id; its mod actions may double-record"),
        }
    }

    let presence_slot = config.wake_url.is_some().then(crate::gateway::PresenceSlot::default);
    let gateway = crate::gateway::spawn_gateway(
        token.clone(),
        GatewayConfig {
            mode: config.gateway_mode,
            default_guild,
            secondary_guilds: topology.secondary.clone(),
            mention_keywords: config.mention_keywords.clone(),
            mention_store: mention_store.clone(),
            archive: archive_store.clone(),
            modlog: modlog_store.clone(),
            crow_bot_ids,
            fanout: primary_fanout,
            // the primary bot lives in the secondaries too, so this gateway carries them
            broadcast_guilds: default_guild.into_iter().chain(topology.secondary.iter().copied()).collect(),
            wake: crate::wake::WakeSender::from_config(config.wake_url.as_deref(), config.wake_secret.as_deref()),
            wake_dm_from: parse_wake_dm_from(&config.wake_dm_from),
            presence: presence_slot.clone(),
        },
    );

    // the secondaries only go live through the observer's own gateway - a separate invisible
    // socket that watches but hears nothing back. it wakes for either the wall (broadcast) or
    // the archive (persist); the fanout is wall-only, the store is archive-only.
    let observe_secondaries = wall_enabled || config.archive;
    let observer_token = config
        .readonly_discord_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let observer_gateway = match (observe_secondaries, observer_token, &observer) {
        (true, Some(observer_token), Some(observer)) => crate::gateway::spawn_gateway(
            observer_token,
            GatewayConfig {
                mode: GatewayMode::Presence,
                default_guild: None,
                secondary_guilds: Vec::new(),
                mention_keywords: Vec::new(),
                mention_store: None,
                // the observer never writes the modlog - moderation is primary-guild only
                modlog: None,
                crow_bot_ids: Vec::new(),
                archive: archive_store.clone(),
                fanout: wall_enabled.then(|| WallFanout {
                    client: observer.client.clone(),
                    cache: enrich_cache.clone(),
                    tx: wall_tx.clone(),
                }),
                broadcast_guilds: observer.guilds.clone(),
                // the observer never taps - the perch answers to the primary guild only
                wake: None,
                wake_dm_from: Vec::new(),
                presence: None,
            },
        ),
        _ => None,
    };

    let wall_state = if wall_enabled {
        // wall implies needs_ledger, so the handle is always open here.
        let layout = ledger
            .as_ref()
            .context("wall enabled without an open ledger")?
            .layout();
        Some(WallState {
            tx: wall_tx.clone(),
            cache: enrich_cache.clone(),
            pool: ClientPool {
                client: DiscordClient::new(&token),
                readonly_client: observer.as_ref().map(|observer| observer.client.clone()),
                default_guild,
                secondary_guilds: topology.secondary.clone(),
                readonly_guilds: observer.as_ref().map(|observer| observer.guilds.clone()).unwrap_or_default(),
            },
            layout,
        })
    } else {
        None
    };

    let allowed_hosts = allowed_hosts(&config);
    let allowed_origins = config.allowed_origins.clone();
    let auth = Arc::new(AuthConfig::new(
        config.public_base_url.clone(),
        &config.auth_tokens,
    ));
    let cancellation = CancellationToken::new();

    // the tempban timer only runs where the daemon lives - stdio sessions are too
    // short-lived to be trusted with an expiry.
    let scheduler = modlog_store
        .clone()
        .map(|modlog| crate::scheduler::spawn_scheduler(DiscordClient::new(&token), modlog));
    if scheduler.is_some() {
        tracing::info!("tempban scheduler ticking");
    }

    let upload_store = UploadStore::new(UPLOAD_TTL);
    let http_config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_hosts.clone())
        .with_allowed_origins(allowed_origins.clone())
        .with_cancellation_token(cancellation.child_token());
    let factory_store = upload_store.clone();
    let factory_observer = observer.clone();
    let factory_message = archive_store.clone();
    let factory_modlog = modlog_store.clone();
    let factory_senders = senders.clone();
    let factory_wake_dm_from = parse_wake_dm_from(&config.wake_dm_from);
    let factory_presence = presence_slot.clone();
    let factory_secondaries = topology.secondary.clone();
    let service: StreamableHttpService<KurouServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || {
                Ok(KurouServer::new(
                    DiscordClient::new(&token),
                    factory_observer.clone(),
                    default_guild,
                    mention_store.clone(),
                    factory_message.clone(),
                    factory_modlog.clone(),
                    factory_store.clone(),
                    factory_senders.clone(),
                )
                .with_secondary_guilds(factory_secondaries.clone())
                .with_wake_dm_from(factory_wake_dm_from.clone())
                .with_presence(factory_presence.clone()))
            },
            Default::default(),
            http_config,
        );

    let upload_route = Router::new()
        .route("/upload", post(crate::uploads::upload_handler))
        .layer(DefaultBodyLimit::max(crate::uploads::MAX_UPLOAD_BYTES))
        .with_state(upload_store);

    let mcp_router = Router::new()
        .nest_service("/mcp", service)
        .merge(upload_route);
    let mcp_router = if auth.is_enabled() {
        let auth_for_middleware = auth.clone();
        tracing::info!(
            tokens = auth.token_count(),
            public_base_url = %config.public_base_url,
            "http bearer auth enabled"
        );
        mcp_router.layer(middleware::from_fn(move |request, next| {
            crate::auth::auth_middleware(auth_for_middleware.clone(), request, next)
        }))
    } else {
        tracing::warn!("http bearer auth is disabled because AUTH_TOKENS is empty");
        mcp_router
    };
    let metadata_router = if auth.is_enabled() {
        let access_token = auth
            .oauth_token(config.oauth_token_label.as_deref())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "OAUTH_TOKEN_LABEL={} did not match any AUTH_TOKENS label",
                    config.oauth_token_label.as_deref().unwrap_or("")
                )
            })?;
        let oauth_store = crate::oauth::OAuthStore::new(access_token, config.public_base_url);
        tracing::info!(
            label = config
                .oauth_token_label
                .as_deref()
                .unwrap_or("<first-token>"),
            "oauth authorization-code shim enabled"
        );
        Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                get(crate::oauth::metadata),
            )
            .route(
                "/.well-known/oauth-protected-resource",
                get(crate::oauth::protected_resource),
            )
            .route(
                "/authorize",
                get(crate::oauth::authorize_get).post(crate::oauth::authorize_post),
            )
            .route("/token", axum::routing::post(crate::oauth::token))
            .with_state(oauth_store)
    } else {
        if config.oauth_token_label.is_some() {
            tracing::warn!(
                label = config.oauth_token_label.as_deref(),
                "OAUTH_TOKEN_LABEL is ignored because AUTH_TOKENS is empty"
            );
        }
        Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                get(crate::auth::protected_resource),
            )
            .with_state(auth)
    };
    let mut router = Router::new().merge(mcp_router).merge(metadata_router);
    if let Some(wall_state) = wall_state {
        // no bearer layer here on purpose - the wall trusts nginx's authelia forward-auth
        // and binds where only the proxy can reach it. the crow guards nothing itself.
        tracing::info!("nightwatch wall mounted at /wall");
        router = router.merge(crate::wall::router(wall_state));
    }
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;

    tracing::info!(
        bind = %bind_addr,
        endpoint = %format!("http://{bind_addr}/mcp"),
        allowed_hosts = ?allowed_hosts,
        allowed_origins = ?allowed_origins,
        "kurou streamable http listening"
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            if let Err(error) = tokio::signal::ctrl_c().await {
                tracing::warn!(%error, "failed to listen for shutdown signal");
            }
            cancellation.cancel();
        })
        .await?;

    if let Some(gateway) = gateway {
        gateway.abort();
    }
    if let Some(observer_gateway) = observer_gateway {
        observer_gateway.abort();
    }
    if let Some(scheduler) = scheduler {
        scheduler.abort();
    }

    Ok(())
}

fn build_modlog_notifier(config: &Config, token: &str) -> Result<Option<crate::modlog::Notifier>> {
    let Some(raw) = config.modlog_channel.as_deref().map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    let id = raw
        .parse::<u64>()
        .with_context(|| format!("MODLOG_CHANNEL '{raw}' is not a valid channel snowflake"))?;
    Ok(Some(crate::modlog::Notifier { client: DiscordClient::new(token), channel: serenity::model::id::ChannelId::new(id) }))
}

// a typo here would kill the summons silently, so bad ids get a warning, not a shrug
fn parse_wake_dm_from(ids: &[String]) -> Vec<UserId> {
    ids.iter().map(|id| id.trim()).filter(|id| !id.is_empty()).filter_map(|id| match id.parse::<u64>() {
        Ok(id) => Some(UserId::new(id)),
        Err(_) => { tracing::warn!(id, "WAKE_DM_FROM entry is not a user id snowflake; skipped"); None }
    }).collect()
}

fn parse_guild_id(name: &str, raw: &str) -> Result<GuildId> {
    let id = raw
        .trim()
        .parse::<u64>()
        .with_context(|| format!("{name} '{raw}' is not a valid snowflake"))?;
    Ok(GuildId::new(id))
}

fn parse_guild_list(name: &str, raw: &[String]) -> Result<Vec<GuildId>> {
    raw.iter()
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| parse_guild_id(name, raw))
        .collect()
}

fn parse_topology(config: &Config) -> Result<GuildTopology> {
    let primary = config
        .primary_guild_raw()
        .map(|raw| parse_guild_id("PRIMARY_GUILD", raw))
        .transpose()?;
    let secondary = parse_guild_list("SECONDARY_GUILDS", &config.secondary_guilds)?;
    let readonly = parse_guild_list("READONLY_GUILDS", &config.readonly_guilds)?;
    if primary.is_none() && (!secondary.is_empty() || !readonly.is_empty()) {
        anyhow::bail!("SECONDARY_GUILDS/READONLY_GUILDS are set but PRIMARY_GUILD (the home guild) is not");
    }
    if let Some(primary) = primary
        && (secondary.contains(&primary) || readonly.contains(&primary))
    {
        anyhow::bail!("PRIMARY_GUILD {primary} also appears in SECONDARY_GUILDS or READONLY_GUILDS; a guild wears one hat");
    }
    if let Some(guild) = secondary.iter().find(|guild| readonly.contains(guild)) {
        anyhow::bail!("guild {guild} is in both SECONDARY_GUILDS and READONLY_GUILDS; writable and read-only can't both be true");
    }
    Ok(GuildTopology { primary, secondary, readonly })
}

fn build_observer(config: &Config, topology: &GuildTopology) -> Result<Option<Observer>> {
    if topology.readonly.is_empty() {
        return Ok(None);
    }
    let token = config
        .readonly_discord_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .context("READONLY_GUILDS is set but READONLY_DISCORD_TOKEN (the observer bot) is not")?;
    Ok(Some(Observer { client: DiscordClient::new(token), guilds: topology.readonly.clone() }))
}

fn allowed_hosts(config: &Config) -> Vec<String> {
    if !config.allowed_hosts.is_empty() {
        return config.allowed_hosts.clone();
    }

    let host = config.host.to_string();
    let bind_addr = std::net::SocketAddr::new(config.host, config.port).to_string();
    vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
        host,
        bind_addr,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uploads::UploadStore;

    fn server(senders: HashMap<String, DiscordClient>) -> KurouServer {
        KurouServer::new(DiscordClient::new("koma-token"), None, None, None, None, None, UploadStore::new(UPLOAD_TTL), Arc::new(senders))
    }

    #[test]
    fn sender_resolution_follows_token_ownership() {
        let crow = server(HashMap::from([("mecha".to_string(), DiscordClient::new("mecha-token"))]));
        assert!(crow.sender_for("koma").is_ok());
        assert!(crow.sender_for("mecha").is_ok());
        let refusal = crow.sender_for("pyonka").unwrap_err();
        assert!(refusal.contains("read-only"));
        assert!(refusal.contains("DISCORD_TOKEN_PYONKA"));
    }

    fn config(args: &[&str]) -> Config {
        use clap::Parser;
        Config::try_parse_from(std::iter::once("kurou").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn topology_reads_primary_with_legacy_fallback() {
        let fresh = parse_topology(&config(&["--primary-guild", "111"])).unwrap();
        assert_eq!(fresh.primary, Some(GuildId::new(111)));

        let legacy = parse_topology(&config(&["--discord-guild-id", "222"])).unwrap();
        assert_eq!(legacy.primary, Some(GuildId::new(222)));

        let both = parse_topology(&config(&["--primary-guild", "111", "--discord-guild-id", "222"])).unwrap();
        assert_eq!(both.primary, Some(GuildId::new(111)));
    }

    #[test]
    fn topology_rejects_orphan_lists_and_double_hats() {
        assert!(parse_topology(&config(&["--secondary-guild", "333"])).is_err());
        assert!(parse_topology(&config(&["--readonly-guild", "444"])).is_err());
        assert!(parse_topology(&config(&["--primary-guild", "111", "--secondary-guild", "111"])).is_err());
        assert!(parse_topology(&config(&["--primary-guild", "111", "--readonly-guild", "111"])).is_err());
        assert!(
            parse_topology(&config(&["--primary-guild", "111", "--secondary-guild", "333", "--readonly-guild", "333"]))
                .is_err()
        );
    }

    #[test]
    fn topology_splits_the_three_hats() {
        let topology = parse_topology(&config(&[
            "--primary-guild", "111",
            "--secondary-guild", "333,334",
            "--readonly-guild", "444",
        ]))
        .unwrap();
        assert_eq!(topology.primary, Some(GuildId::new(111)));
        assert_eq!(topology.secondary, vec![GuildId::new(333), GuildId::new(334)]);
        assert_eq!(topology.readonly, vec![GuildId::new(444)]);
    }

    #[test]
    fn writable_covers_primary_and_secondaries_only() {
        let crow = server(HashMap::new());
        let crow = KurouServer {
            default_guild: Some(GuildId::new(111)),
            ..crow
        }
        .with_secondary_guilds(vec![GuildId::new(333)]);
        assert!(crow.is_writable(GuildId::new(111)));
        assert!(crow.is_writable(GuildId::new(333)));
        assert!(!crow.is_writable(GuildId::new(444)));
    }
}
