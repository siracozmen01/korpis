// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Korpis control plane: the client API adapter and the composition root that wires the
//! PostgreSQL store into the application (§1 of ARCHITECTURE.md).

#![warn(missing_docs)]

pub mod api;
pub mod bodies;
pub mod data;
pub mod install;
pub mod keys;
pub mod tls;

use api::Api;
use connectrpc::Server;
use korpis_app::App;
use korpis_app::ports::SystemClock;
use korpis_recipe::registry::Registry;
use korpis_recipe::source::RegistrySource;
use korpis_store::PgStore;
use std::net::SocketAddr;
use std::sync::Arc;

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
    .await?;
    store.ensure_partitions(3).await?;
    let keys = keys::Keys::load_or_create(&config.data)?;
    let recipes = RegistrySource::new(Registry::new(config.plain_http_registries.clone()));
    let app = App::new(store, SystemClock, keys.token.clone(), recipes);
    app.register_keys(&keys.public()).await?;
    let tls = tls::server_config(&config.data)?;
    let bound = Server::bind(config.listen).await.map_err(|e| ServeError::Serve(e.to_string()))?;
    let addr = bound.local_addr()?;
    let bound = bound.with_tls(tls);
    let router = Arc::new(Api::new(app)).router();
    let task = tokio::spawn(async move {
        bound.serve(router).await.map_err(|e| ServeError::Serve(e.to_string()))
    });
    tracing::info!(%addr, "serving");
    Ok(Running { addr, task })
}
