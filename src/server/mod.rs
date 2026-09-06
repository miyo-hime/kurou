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
use serenity::model::id::GuildId;
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

#[derive(Clone, Debug)]
pub struct KurouServer {
    pub(crate) client: DiscordClient,
    pub(crate) observer: Option<Observer>,
    pub(crate) default_guild: Option<GuildId>,
    pub(crate) mention_store: Option<MentionStore>,
    pub(crate) message_store: Option<MessageStore>,
    pub(crate) modlog_store: Option<ModlogStore>,
    pub(crate) upload_store: UploadStore,
    pub(crate) senders: Arc<HashMap<String, DiscordClient>>,
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
            mention_store,
            message_store,
            modlog_store,
            upload_store,
            senders,
            tool_router: Self::tool_router(),
        }
    }

    pub(crate) fn readonly_guilds(&self) -> &[GuildId] {
        self.observer.as_ref().map(|observer| observer.guilds.as_slice()).unwrap_or(&[])
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

    // guild is known: primary token for the primary guild, observer for a secondary.
    pub(crate) fn client_for_guild(&self, guild: GuildId) -> &DiscordClient {
        match &self.observer {
            Some(observer) if Some(guild) != self.default_guild => &observer.client,
            _ => &self.client,
        }
    }

    // only a channel id in hand. with no observer it's always primary; otherwise probe
    // once - the primary bot sees its own guild's channels and 403s on the secondaries.
    pub(crate) async fn client_for_channel(
        &self,
        channel: serenity::model::id::ChannelId,
    ) -> &DiscordClient {
        let Some(observer) = &self.observer else {
            return &self.client;
        };
        match self.client.channel_guild(channel).await {
            Ok(guild) if guild == self.default_guild => &self.client,
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
            + tools::users::router()
            + tools::modlog::router()
            + tools::hands::router()
            + tools::raid::router()
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "kurou",
    version = "0.15.0",
    instructions = "a small window into a discord server. crow on the wire. reads: list_servers, get_server_info, list_channels, list_threads, read_messages (anchor with around/before/after), get_message, get_pinned, scan_channel (deep author/mention/text sweep). archive: search_messages (full-text search the local message archive, needs ARCHIVE=true). voice: send_message, get_user_id_by_name. mentions: check_mentions, mark_mentions_seen. mod ledger: check_ledger, user_history - the crow's moderation memory, every action it witnessed or performed. mod hands (primary guild only, caller's own bot, intent required, every act recorded): ban_user, unban_user, kick_user, timeout_user, untimeout_user, warn_user, revoke_warn, add_role, remove_role, set_nickname, delete_message, lock_channel, unlock_channel, set_slowmode, purge_channel, delete_invite; get_bans and list_invites are open reads. the watcher also records what other moderators do: bans, unbans, kicks, timeouts, joins and leaves land as observed ledger rows. read-only secondary guilds ride a separate observer bot, routed for you. multi-identity: every caller is a labeled bearer - reads are open to all sisters, send_message and the mod hands act with the caller's own bot voice or refuse, and the mention inbox answers only to koma."
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
    let default_guild = config
        .discord_guild_id
        .as_deref()
        .map(parse_guild_id)
        .transpose()?;
    let observer = build_observer(&config, default_guild)?;

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

    let gateway = crate::gateway::spawn_gateway(
        token.clone(),
        GatewayConfig {
            mode: config.gateway_mode,
            default_guild,
            mention_keywords: config.mention_keywords.clone(),
            mention_store: mention_store.clone(),
            archive: None,
            modlog: modlog_store.clone(),
            crow_bot_ids: Vec::new(),
            fanout: None,
            broadcast_guilds: Vec::new(),
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
    let default_guild = config
        .discord_guild_id
        .as_deref()
        .map(parse_guild_id)
        .transpose()?;
    let observer = build_observer(&config, default_guild)?;
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

    let gateway = crate::gateway::spawn_gateway(
        token.clone(),
        GatewayConfig {
            mode: config.gateway_mode,
            default_guild,
            mention_keywords: config.mention_keywords.clone(),
            mention_store: mention_store.clone(),
            archive: archive_store.clone(),
            modlog: modlog_store.clone(),
            crow_bot_ids,
            fanout: primary_fanout,
            broadcast_guilds: default_guild.into_iter().collect(),
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
                ))
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

fn parse_guild_id(raw: &str) -> Result<GuildId> {
    let id = raw
        .trim()
        .parse::<u64>()
        .with_context(|| format!("DISCORD_GUILD_ID '{raw}' is not a valid snowflake"))?;
    Ok(GuildId::new(id))
}

fn build_observer(config: &Config, default_guild: Option<GuildId>) -> Result<Option<Observer>> {
    let guilds = config
        .readonly_guilds
        .iter()
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| {
            raw.trim()
                .parse::<u64>()
                .map(GuildId::new)
                .with_context(|| format!("READONLY_GUILDS '{raw}' is not a valid snowflake"))
        })
        .collect::<Result<Vec<_>>>()?;
    if guilds.is_empty() {
        return Ok(None);
    }
    if default_guild.is_none() {
        anyhow::bail!("READONLY_GUILDS is set but DISCORD_GUILD_ID (the primary, the only place send_message may post) is not");
    }
    let token = config
        .readonly_discord_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .context("READONLY_GUILDS is set but READONLY_DISCORD_TOKEN (the observer bot) is not")?;
    Ok(Some(Observer { client: DiscordClient::new(token), guilds }))
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
}
