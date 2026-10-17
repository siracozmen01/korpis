// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Korpis control plane: the client API adapter and the composition root that wires the
//! PostgreSQL store into the application (§1 of ARCHITECTURE.md).

#![warn(missing_docs)]

mod about;
pub mod accept;
pub mod acme;
pub mod api;
pub mod authority;
pub mod bodies;
mod flags;
pub use flags::{placement_policy, span};
mod background;
mod buckets;
mod control_jobs;
pub mod data;
pub mod factor_vault;
pub mod feasibility;
pub mod front;
pub mod install;
pub mod issuer;
pub mod keys;
pub mod lookup;
pub mod named_relays;
pub mod passwords;
mod provider_calls;
pub mod providers;
mod reachability;
pub mod records;
pub mod relay;
pub mod relying_party;
mod repos;
pub mod revocations;
pub mod sealer;
pub mod sessions;
pub mod standalone;
#[cfg(feature = "testing")]
pub mod testing;
pub mod tls;
pub mod tls_vault;
pub mod vault;
mod web;

use api::Api;
use korpis_app::App;
use korpis_app::node::SessionTerms;
use korpis_app::ports::SystemClock;
use korpis_core::schedule::PlacementPolicy;
use korpis_egress::Reach;
use korpis_recipe::registry::Registry;
use korpis_recipe::source::RegistrySource;
use korpis_store::PgStore;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// What `serve` needs.
#[derive(Debug, Clone)]
pub struct ServeConfig {
    /// Keys and TLS material.
    pub data: data::DataDir,
    /// Connects as, or assumes, the application role; migrations are `install`'s job.
    pub database_url: String,
    /// Where to listen.
    pub listen: SocketAddr,
    /// The connection pool's size.
    pub max_connections: u32,
    /// Registries spoken to over plain HTTP, as `host:port`: development registries on
    /// loopback only. Every other registry is HTTPS.
    pub plain_http_registries: Vec<String>,
    /// Networks a registry a recipe names may be on besides the public internet, for a private
    /// mirror (§3 of docs/17-security.md).
    pub fetch_allow: Vec<ipnet::IpNet>,
    /// The installation's placement policy (§2.2 of docs/05-scheduling.md).
    pub placement_policy: PlacementPolicy,
    /// How often workloads no node could take are tried again.
    pub placement_retry: Duration,
    /// How often a watcher re-reads what it watches when nothing was announced.
    pub watch_resync: Duration,
    /// How often agents send their heartbeat; three missed make a node unreachable.
    pub heartbeat_interval: Duration,
    /// How long a lease lasts from the holder's request (§4.5 of docs/02-architecture.md).
    pub lease_term: Duration,
    /// The window repeated identical denials are coalesced over (§10 of docs/10-api.md).
    pub denial_window: Duration,
    /// How long the event log keeps an event (§5 of docs/15-observability.md).
    pub event_retention: Duration,
    /// The operator's floor on how long offloaded stream segments are kept, disclosed to every
    /// tenant with the stream (§8 of docs/17-security.md); zero is none.
    pub stream_retention_floor: Duration,
    /// Whether this process serves the relay role (§3.1 of docs/02-architecture.md); off where
    /// relays run as their own processes and the control plane should carry no sessions.
    pub relay_role: bool,
    /// The load balancers whose connections open with a PROXY header naming the client (§3.1 of
    /// docs/02-architecture.md); none reads no header from anyone.
    pub proxy_protocol_from: Vec<ipnet::IpNet>,
    /// The DNS server domains are verified through (§3.2 of docs/07-networking.md); the
    /// system's resolvers when none.
    pub dns_resolver: Option<std::net::SocketAddr>,
    /// The ACME directory `acme` certificates are ordered from, and as whom (§3.2 of
    /// docs/07-networking.md).
    pub acme: acme::AcmeConfig,
    /// The rates of the client API's buckets (§10 of docs/10-api.md).
    pub rates: korpis_core::rate::Rates,
    /// The site passkeys are bound to and the origins a ceremony may come from (§9.6 of
    /// docs/08-identity.md); without one, no passkey can be registered or used.
    pub relying_party: Option<korpis_core::webauthn::RelyingParty>,
    /// How long user sessions last (§9.4 of docs/08-identity.md).
    pub session_limits: korpis_core::account::session::SessionLimits,
    /// The panel's static bundle, served on the API's address (§1 of docs/11-surface-web.md);
    /// without one, the address serves the API alone.
    pub web_root: Option<std::path::PathBuf>,
}

/// Why serving failed.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// Connecting or maintenance failed.
    #[error("store: {0}")]
    Store(#[from] korpis_store::Error),
    /// The signing keys could not be loaded.
    #[error("keys: {0}")]
    Keys(#[from] keys::KeyError),
    /// The TLS material could not be loaded.
    #[error("TLS: {0}")]
    Tls(#[from] tls::TlsError),
    /// Registering the public keys failed.
    #[error("{0}")]
    App(#[from] korpis_app::AppError),
    /// The listener failed.
    #[error("listening: {0}")]
    Io(#[from] std::io::Error),
    /// The server failed.
    #[error("serving: {0}")]
    Serve(String),
    /// The configuration does not parse.
    #[error("configuration: {0}")]
    Config(String),
}

/// A bound, running control plane.
#[derive(Debug)]
pub struct Running {
    /// Where it listens.
    pub addr: SocketAddr,
    /// The server; it ends only on failure.
    pub task: tokio::task::JoinHandle<Result<(), ServeError>>,
    /// The services' state, for a host that embeds the control plane: its use cases, called
    /// in-process, such as the ones only the control plane's own work calls.
    pub api: Arc<Api>,
}

/// The control plane's use cases over `store`, holding `keys` and what `config` gives them.
fn application(
    config: &ServeConfig,
    store: PgStore,
    keys: &keys::Keys,
) -> Result<api::ServerApp, ServeError> {
    let reach = Reach::with(config.fetch_allow.clone());
    let registry = Registry::reaching(config.plain_http_registries.clone(), reach)
        .map_err(|e| ServeError::Serve(format!("the registry client: {e}")))?;
    let recipes = RegistrySource::new(registry);
    let terms = SessionTerms {
        heartbeat_interval: config.heartbeat_interval,
        lease_term: config.lease_term,
        ..Default::default()
    };
    let app = App::new(store, SystemClock, keys.token.clone(), recipes)
        .with_placement_policy(config.placement_policy.clone())
        .with_session_terms(terms)
        .with_denial_window(config.denial_window)
        .with_rates(config.rates)
        .with_stream_floor(config.stream_retention_floor)
        .with_certificate_issuer(Arc::new(authority::Authority::load(&config.data)?))
        .with_assignment_sealer(Arc::new(sealer::Ed25519Sealer::new(
            keys.assignment.clone(),
            keys.assignment_key_id.clone(),
        )))
        .with_vault(Arc::new(vault::Vault::load_or_create(&config.data)?))
        .with_tls(Arc::new(tls_vault::TlsKeys::load_or_create(&config.data)?))
        .with_acme(Arc::new(acme::AcmeClient::new(config.acme.clone(), &config.data)))
        .with_timer(Arc::new(issuer::TokioTimer))
        .with_lookup(Arc::new(
            lookup::Lookup::new(config.dns_resolver)
                .map_err(|e| ServeError::Config(format!("DNS resolver: {e}")))?,
        ))
        .with_factor_vault(Arc::new(factor_vault::FactorKeys::load_or_create(&config.data)?))
        .with_passwords(Arc::new(passwords::Bounded::per_core()))
        .with_session_limits(config.session_limits);
    if let Some(rp) = &config.relying_party {
        return Ok(app.with_relying_party(rp.clone()));
    }
    tracing::warn!("no --public-url: passkeys cannot be registered or used");
    Ok(app)
}

/// Binds and starts serving. Returns once the listener is bound, so callers (and tests) can
/// connect immediately.
pub async fn serve(config: ServeConfig) -> Result<Running, ServeError> {
    let store = PgStore::connect(
        &config.database_url,
        config.max_connections,
        Arc::new(bodies::ProtoJsonBodies),
    )
    .await?
    .with_watch_resync(config.watch_resync);
    store.listen();
    store.run_sequencer();
    store.ensure_partitions(3).await?;
    tokio::spawn(background::prune_events(store.clone(), config.event_retention));
    let keys = keys::Keys::load_or_create(&config.data)?;
    let app = application(&config, store, &keys)?;
    app.register_keys(&keys.public()).await?;
    let forgotten = app.forget_connections().await?;
    tracing::info!(nodes = forgotten, "nodes unknown until heard again");
    let tls = tls::server_config(&config.data)?;
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let addr = listener.local_addr()?;
    let api = Api::new(app).with_ca_fingerprint(tls::ca_fingerprint(&config.data)?);
    let api = Arc::new(api.with_relay_role(config.relay_role));
    let token_keys = {
        use korpis_app::ports::EnrollmentRead as _;
        api.app().store().verify_keys("token").await.map_err(korpis_app::AppError::from)?
    };
    let names = token_keys.iter().map(|k| (k.key_id.as_str(), k.public.as_slice()));
    api.relay().trust(relay::token_ring(names));
    background::start(&api, &config);
    let charging = api::Charging::new(Arc::clone(&api));
    let service = connectrpc::ConnectRpcService::new(Arc::clone(&api).router());
    let service = front(service.with_interceptor(charging), &config);
    let proxies: Arc<[ipnet::IpNet]> = config.proxy_protocol_from.clone().into();
    let task = tokio::spawn(async move {
        accept::serve_on(listener, tls, service, proxies).await.map_err(ServeError::Serve)
    });
    tracing::info!(%addr, "serving");
    Ok(Running { addr, task, api })
}

/// The address's front: the API, the panel's files from `--web-root`, and cross-origin calls
/// answered for the relying party's origins, the installation's own and each `--web-origin`.
fn front(api: connectrpc::ConnectRpcService, config: &ServeConfig) -> front::Front {
    if config.web_root.is_none() {
        tracing::info!("no --web-root: this address serves the API alone, without the panel");
    }
    let origins = config.relying_party.as_ref().map(|rp| rp.origins.clone()).unwrap_or_default();
    front::Front::new(api, config.web_root.clone(), origins)
}
