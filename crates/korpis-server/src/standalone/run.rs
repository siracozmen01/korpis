// SPDX-License-Identifier: AGPL-3.0-or-later

//! Running a joined relay (§3.1 of docs/02-architecture.md): it serves `RelayService` and
//! `DataService` with the certificate it was issued, renewing it as it nears expiry, verifying by
//! what its feed sends, and holds no database. It stops when the control plane refuses its feed, since a removed relay must not
//! go on verifying by what it held.

use super::RelayDir;
use super::fed::Fed;
use super::follow::{FollowError, follow};
use super::renew::keep_renewed;
use super::served::{Served, load};
use crate::api::RelayRole;
use connectrpc::Router;
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
    /// The certificate it serves.
    pub served: Arc<Served>,
    /// The relay; it ends only on failure or refusal.
    pub task: tokio::task::JoinHandle<Result<(), RunError>>,
}

/// The URL a relay serves at, as it joined with it.
pub fn url(dir: &RelayDir) -> Result<String, RunError> {
    Ok(std::fs::read_to_string(dir.file("url"))?.trim().to_owned())
}

/// How a relay meets its clients beyond TLS.
#[derive(Debug, Clone, Default)]
pub struct Edge {
    /// The load balancers whose connections open with a PROXY header naming the client.
    pub proxies: Vec<ipnet::IpNet>,
    /// The panel's origins, whose calls are answered cross-origin (§3.1 of
    /// docs/02-architecture.md): a browser opens its sessions here.
    pub web_origins: Vec<String>,
}

/// Runs the relay kept in `dir` on `listener`, meeting clients as `edge` says. Returns once it is
/// serving.
pub async fn run(
    dir: RelayDir,
    listener: TcpListener,
    edge: Edge,
) -> Result<RunningRelay, RunError> {
    let Some(relay) = dir.joined()? else { return Err(RunError::NotJoined) };
    let served = Arc::new(Served::new(load(&dir.tls_files(dir.tls_generation()?))?));
    let resolver: Arc<dyn rustls::server::ResolvesServerCert> = served.clone();
    let tls = crate::tls::resolving(resolver)?;
    let fed = Arc::new(Fed::new(url(&dir)?));
    let addr = listener.local_addr()?;
    let router = Arc::new(RelayRole::new(Arc::clone(&fed))).register(Router::new());
    let service = connectrpc::ConnectRpcService::new(router);
    let service = crate::front::Front::new(service, None, edge.web_origins);
    let renewing = tokio::spawn(keep_renewed(dir.clone(), relay, Arc::clone(&served)));
    let following = tokio::spawn(follow(dir, Arc::clone(&fed)));
    let task = tokio::spawn(async move {
        let ended = tokio::select! {
            served = crate::accept::serve_on(listener, tls, service, edge.proxies.into()) => {
                served.map_err(RunError::Serve)
            }
            refused = following => match refused {
                Ok(e) => Err(RunError::Follow(e)),
                Err(e) => Err(RunError::Serve(e.to_string())),
            },
        };
        renewing.abort();
        ended
    });
    tracing::info!(%addr, "relay serving");
    Ok(RunningRelay { addr, fed, served, task })
}
