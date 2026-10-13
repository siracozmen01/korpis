// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Korpis control plane: the client API adapter and the composition root that wires the
//! PostgreSQL store into the application (§1 of ARCHITECTURE.md).

#![warn(missing_docs)]

pub mod api;
pub mod bodies;
pub mod data;
pub mod install;
pub mod keys;
mod provider_calls;
pub mod providers;
pub mod sealer;
pub mod sessions;
pub mod tls;

use api::Api;
use connectrpc::Server;
use korpis_app::App;
use korpis_app::node::SessionTerms;
use korpis_app::ports::SystemClock;
use korpis_core::schedule::PlacementPolicy;
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
    let recipes = RegistrySource::new(Registry::new(config.plain_http_registries.clone()));
    let terms = SessionTerms {
        heartbeat_interval: config.heartbeat_interval,
        lease_term: config.lease_term,
        ..Default::default()
    };
    let app = App::new(store, SystemClock, keys.token.clone(), recipes)
        .with_placement_policy(config.placement_policy.clone())
        .with_session_terms(terms)
        .with_denial_window(config.denial_window)
        .with_assignment_sealer(Arc::new(sealer::Ed25519Sealer::new(
            keys.assignment.clone(),
            keys.assignment_key_id.clone(),
        )));
    app.register_keys(&keys.public()).await?;
    let forgotten = app.forget_connections().await?;
    tracing::info!(nodes = forgotten, "nodes unknown until heard again");
    let tls = tls::server_config(&config.data)?;
    let bound = Server::bind(config.listen).await.map_err(|e| ServeError::Serve(e.to_string()))?;
    let addr = bound.local_addr()?;
    let bound = bound.with_tls(tls);
    let api = Arc::new(Api::new(app).with_ca_fingerprint(tls::ca_fingerprint(&config.data)?));
    tokio::spawn(retry_placements(Arc::clone(&api), config.placement_retry));
    tokio::spawn(sweep_silent_nodes(Arc::clone(&api), config.heartbeat_interval));
    tokio::spawn(dispatch_changes(Arc::clone(&api)));
    tokio::spawn(track_conditions(Arc::clone(&api)));
    tokio::spawn(provider_calls::keep_registrations(Arc::clone(&api)));
    tokio::spawn(provider_calls::dispatch(Arc::clone(&api)));
    tokio::spawn(close_denial_runs(Arc::clone(&api), config.denial_window));
    let router = api.router();
    let task = tokio::spawn(async move {
        bound.serve(router).await.map_err(|e| ServeError::Serve(e.to_string()))
    });
    tracing::info!(%addr, "serving");
    Ok(Running { addr, task })
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
