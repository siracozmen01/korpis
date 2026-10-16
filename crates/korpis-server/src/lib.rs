// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Korpis control plane: the client API adapter and the composition root that wires the
//! PostgreSQL store into the application (§1 of ARCHITECTURE.md).

#![warn(missing_docs)]

pub mod accept;
pub mod acme;
pub mod api;
pub mod authority;
pub mod bodies;
mod buckets;
mod control_jobs;
pub mod data;
pub mod factor_vault;
pub mod feasibility;
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
}

/// A placement policy from its JSON form, a `korpis.v1.PlacementPolicy`.
pub fn placement_policy(json: &str) -> Result<PlacementPolicy, ServeError> {
    let message: korpis_proto::korpis::v1::PlacementPolicy = serde_json::from_str(json)
        .map_err(|e| ServeError::Config(format!("placement policy: {e}")))?;
    korpis_wire::placement::policy_from_pb("placement_policy", &message)
        .map_err(|e| ServeError::Config(format!("{}: {}", e.field, e.message)))
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
        .with_passwords(Arc::new(passwords::Bounded::per_core()));
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
    tokio::spawn(prune_events(store.clone(), config.event_retention));
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
    background(&api, &config);
    let charging = api::Charging::new(Arc::clone(&api));
    let service = connectrpc::ConnectRpcService::new(Arc::clone(&api).router());
    let service = service.with_interceptor(charging);
    let proxies: Arc<[ipnet::IpNet]> = config.proxy_protocol_from.clone().into();
    let task = tokio::spawn(async move {
        accept::serve_on(listener, tls, service, proxies).await.map_err(ServeError::Serve)
    });
    tracing::info!(%addr, "serving");
    Ok(Running { addr, task, api })
}

/// Starts the control plane's own work, each task ending with the process.
fn background(api: &Arc<Api>, config: &ServeConfig) {
    tokio::spawn(retry_placements(Arc::clone(api), config.placement_retry));
    tokio::spawn(sweep_silent_nodes(Arc::clone(api), config.heartbeat_interval));
    tokio::spawn(dispatch_changes(Arc::clone(api)));
    tokio::spawn(track_conditions(Arc::clone(api)));
    tokio::spawn(provider_calls::keep_registrations(Arc::clone(api)));
    tokio::spawn(provider_calls::dispatch(Arc::clone(api)));
    tokio::spawn(revocations::keep(Arc::clone(api)));
    tokio::spawn(named_relays::keep(Arc::clone(api)));
    tokio::spawn(alert_certificates(Arc::clone(api)));
    tokio::spawn(reachability::keep(Arc::clone(api)));
    tokio::spawn(close_denial_runs(Arc::clone(api), config.denial_window));
    tokio::spawn(expire_offloaded(Arc::clone(api)));
    tokio::spawn(control_jobs::queue_steps(Arc::clone(api)));
    tokio::spawn(control_jobs::work(Arc::clone(api)));
    tokio::spawn(issuer::keep(Arc::clone(api)));
    tokio::spawn(records::keep(Arc::clone(api)));
    tokio::spawn(buckets::forget(Arc::clone(api)));
}

/// Tries again, every `every`, each workload whose approved target no node could take: a node
/// enrolled, uncordoned, or freed is found here rather than waiting for the next approval. The
/// task ends with the process.
async fn retry_placements(api: Arc<Api>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().reconcile_placements().await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(tried = n, "placement retry"),
            Err(e) => tracing::warn!(error = %e, "placement retry failed"),
        }
    }
}

/// Marks unreachable, every heartbeat interval, each connected node not heard for three. The
/// task ends with the process.
async fn sweep_silent_nodes(api: Arc<Api>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().sweep_silent_nodes().await {
            Ok(0) => {}
            Ok(n) => tracing::info!(nodes = n, "nodes unreachable: three heartbeats missed"),
            Err(e) => tracing::warn!(error = %e, "node sweep failed"),
        }
    }
}

/// Writes, four times a window, the repeats of every run of denials whose window has closed,
/// so a run that stops repeating is in the log within about a window and a quarter of its last
/// repeat. The task ends with the process.
async fn close_denial_runs(api: Arc<Api>, window: Duration) {
    let mut tick = tokio::time::interval((window / 4).max(Duration::from_millis(250)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().close_denial_runs().await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(runs = n, "denial runs closed"),
            Err(e) => tracing::warn!(error = %e, "closing denial runs failed"),
        }
    }
}

/// Decides workloads' convergence on every announced change and every resync, writing an event
/// when it changed (§5 of docs/15-observability.md). The task ends with the process.
async fn track_conditions(api: Arc<Api>) {
    use korpis_app::ports::{Change, Changes as _, Store as _};
    let mut changes = api.app().store().subscribe();
    let mut next = Some(Change::Resync);
    while let Some(change) = next {
        let decided = match change {
            Change::Workload(w) => api.app().track_convergence(w).await.map(u64::from),
            Change::Resync => api.app().track_all_convergence().await,
        };
        if let Err(e) = decided {
            tracing::warn!(error = %e, "deciding convergence failed");
        }
        next = changes.next().await;
    }
}

/// Reports, hourly, each relay certificate and each ingress certificate that came a step closer to
/// expiring (§7 of docs/18-operations.md, §3.2 of docs/07-networking.md). The task ends with the
/// process.
async fn alert_certificates(api: Arc<Api>) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().alert_relay_certificates().await {
            Ok(0) => {}
            Ok(n) => tracing::warn!(relays = n, "relay certificates nearing expiry"),
            Err(e) => tracing::warn!(error = %e, "checking relay certificates failed"),
        }
        match api.app().alert_certificates().await {
            Ok(0) => {}
            Ok(n) => tracing::warn!(certificates = n, "ingress certificates nearing expiry"),
            Err(e) => tracing::warn!(error = %e, "checking ingress certificates failed"),
        }
    }
}

/// Deletes, hourly, offloaded stream segments past their retention (§4 of docs/14-streams.md).
/// The task ends with the process.
async fn expire_offloaded(api: Arc<Api>) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().expire_offloaded().await {
            Ok(pass) => {
                if pass.deleted > 0 {
                    tracing::info!(segments = pass.deleted, "offloaded segments past retention");
                }
                for (repository, workload, e) in pass.failed {
                    tracing::warn!(%repository, %workload, error = %e,
                        "offloaded segments past their retention were not deleted");
                }
            }
            Err(e) => tracing::warn!(error = %e, "expiring offloaded segments failed"),
        }
    }
}

/// Prunes the event log hourly to its retention window. The task ends with the process.
async fn prune_events(store: PgStore, retention: Duration) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match store.prune_events(retention).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(events = n, "events past retention pruned"),
            Err(e) => tracing::warn!(error = %e, "pruning the event log failed"),
        }
    }
}

/// Wakes the session of every node a workload change concerns, so its set is recomputed and,
/// if it changed, sent; a resync wakes them all. The task ends with the process.
async fn dispatch_changes(api: Arc<Api>) {
    use korpis_app::ports::{Change, Changes as _, Store as _};
    let mut changes = api.app().store().subscribe();
    while let Some(change) = changes.next().await {
        match change {
            Change::Workload(w) => match api.app().nodes_concerned(w).await {
                Ok(nodes) => nodes.into_iter().for_each(|n| api.sessions().wake(n)),
                Err(e) => {
                    tracing::warn!(error = %e, "change dispatch failed; waking every session");
                    api.sessions().wake_all();
                }
            },
            Change::Resync => api.sessions().wake_all(),
        }
    }
}
