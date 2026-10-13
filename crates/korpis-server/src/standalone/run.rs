// SPDX-License-Identifier: AGPL-3.0-or-later

//! Running a joined relay (§3.1 of docs/02-architecture.md): it serves `RelayService` and
//! `DataService` with the certificate it was issued, verifying by what its feed sends, and holds
//! no database. It stops when the control plane refuses its feed, since a removed relay must not
//! go on verifying by what it held.

use super::RelayDir;
use super::fed::Fed;
use super::follow::{FollowError, follow};
use crate::api::RelayRole;
use connectrpc::Router;
use connectrpc::server::Server;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Why a relay could not run, or stopped.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// It has not joined.
    #[error("this relay has not joined; run `korpis-server relay join` first")]
    NotJoined,
    /// Its directory could not be read.
    #[error("the relay directory: {0}")]
    Io(#[from] std::io::Error),
    /// Its certificate or key could not be used.
    #[error("TLS: {0}")]
    Tls(#[from] crate::tls::TlsError),
    /// Following the feed stopped.
    #[error(transparent)]
    Follow(#[from] FollowError),
    /// Serving failed.
    #[error("serving: {0}")]
    Serve(String),
}

/// A running relay.
#[derive(Debug)]
pub struct RunningRelay {
    /// Where it listens.
    pub addr: SocketAddr,
    /// What it verifies by.
    pub fed: Arc<Fed>,
    /// The relay; it ends only on failure or refusal.
    pub task: tokio::task::JoinHandle<Result<(), RunError>>,
}

/// The URL a relay serves at, as it joined with it.
pub fn url(dir: &RelayDir) -> Result<String, RunError> {
    Ok(std::fs::read_to_string(dir.file("url"))?.trim().to_owned())
}

/// Runs the relay kept in `dir` on `listener`. Returns once it is serving.
pub async fn run(dir: RelayDir, listener: TcpListener) -> Result<RunningRelay, RunError> {
    if dir.joined()?.is_none() {
        return Err(RunError::NotJoined);
    }
    let tls = crate::tls::relay_server_config(&dir.file("tls.pem"), &dir.file("tls.key"))?;
    let fed = Arc::new(Fed::new(url(&dir)?));
    let addr = listener.local_addr()?;
    let router = Arc::new(RelayRole::new(Arc::clone(&fed))).register(Router::new());
    let bound = Server::from_listener(listener).with_tls(tls);
    let following = tokio::spawn(follow(dir, Arc::clone(&fed)));
    let task = tokio::spawn(async move {
        tokio::select! {
            served = bound.serve(router) => served.map_err(|e| RunError::Serve(e.to_string())),
            refused = following => match refused {
                Ok(e) => Err(RunError::Follow(e)),
                Err(e) => Err(RunError::Serve(e.to_string())),
            },
        }
    });
    tracing::info!(%addr, "relay serving");
    Ok(RunningRelay { addr, fed, task })
}
