// SPDX-License-Identifier: AGPL-3.0-or-later

//! Running a joined relay (§3.1 of docs/02-architecture.md): it serves `RelayService` and
//! `DataService` with the certificate it was issued, renewing it as it nears expiry, verifying by
//! what its feed sends, and holds no database; and SFTP where its operator turns it on. It stops
//! when the control plane refuses its feed, since a removed relay must not go on verifying by
//! what it held.

use super::RelayDir;
use super::fed::Fed;
use super::feed::SftpReport;
use super::files::AskControl;
use super::follow::{FollowError, follow};
use super::renew::keep_renewed;
use super::served::{Served, load};
use crate::accept::Tls;
use crate::api::RelayRole;
use connectrpc::Router;
use korpis_core::relay::RelayUrl;
use korpis_core::relay::sftp::SftpAddress;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;

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
    /// The relay role it serves, which its SFTP server opens sessions at.
    role: Arc<RelayRole<Fed>>,
    /// What it was started with, to answer SFTP from.
    dir: RelayDir,
    proxies: Arc<[ipnet::IpNet]>,
    /// What it reports serving over SFTP in its feed requests.
    sftp: watch::Sender<SftpReport>,
}

impl RunningRelay {
    /// Serves SFTP on `listener` with `host_key` as well (§3.1 of docs/02-architecture.md),
    /// meeting clients as the relay's API listener does, and reports it in the feed: reached at
    /// `advertise`, or at the host of the relay's URL and the port `listener` is bound to.
    ///
    /// # Errors
    /// The relay's directory cannot be read.
    pub fn serve_sftp(
        &self,
        listener: TcpListener,
        host_key: russh::keys::PrivateKey,
        advertise: Option<SftpAddress>,
    ) -> Result<tokio::task::JoinHandle<std::io::Result<()>>, RunError> {
        let asking = Arc::new(AskControl::new(&self.dir).map_err(RunError::Serve)?);
        let address = if let Some(given) = advertise {
            given
        } else {
            let at = RelayUrl::parse(&url(&self.dir)?).map_err(|e| RunError::Serve(e.0))?;
            SftpAddress::at(&at, listener.local_addr()?.port())
        };
        let blob = host_key.public_key().to_bytes().map_err(|e| RunError::Serve(e.to_string()))?;
        self.sftp.send_replace(Some((blob, address.to_string())));
        let ours = (Arc::clone(&self.role), asking);
        let proxies = Arc::clone(&self.proxies);
        Ok(tokio::spawn(crate::sftp::serve(listener, host_key, proxies, ours)))
    }
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
    let role = Arc::new(RelayRole::new(Arc::clone(&fed)));
    let router = Arc::clone(&role).register(Router::new());
    let service = connectrpc::ConnectRpcService::new(router);
    let service = crate::front::Front::new(service, None, edge.web_origins)
        .with_portal_origins(fed.portal_origins());
    let renewing = tokio::spawn(keep_renewed(dir.clone(), relay, Arc::clone(&served)));
    let (sftp, reported) = watch::channel(None);
    let following = tokio::spawn(follow(dir.clone(), Arc::clone(&fed), reported));
    let proxies: Arc<[ipnet::IpNet]> = edge.proxies.into();
    let serving_proxies = Arc::clone(&proxies);
    let task = tokio::spawn(async move {
        let ended = tokio::select! {
            served = crate::accept::serve_on(listener, Tls::own(tls), service, serving_proxies) => {
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
    Ok(RunningRelay { addr, fed, served, task, role, dir, proxies, sftp })
}
