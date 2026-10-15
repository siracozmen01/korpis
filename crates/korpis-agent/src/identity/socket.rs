// SPDX-License-Identifier: AGPL-3.0-or-later

//! One socket per workload, in a directory of its own the driver mounts read-only at
//! [`MOUNT_POINT`] inside that workload and nowhere else (§6 of docs/02-architecture.md). The
//! socket is the workload's identity: a request on it is forwarded naming the workload it was
//! made for, whoever connected. It speaks Connect over HTTP/1.1 and HTTP/2, so a workload can
//! ask with any HTTP client, `curl --unix-socket` included.

use super::{Credentials, NoCredential};
use connectrpc::{ConnectError, ConnectRpcService, RequestContext, Response, Router};
use connectrpc::{ServiceRequest, ServiceResult};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::WorkloadIdentityServiceExt as _;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;
use tower_service::Service as _;

/// Where a workload finds its socket's directory.
pub const MOUNT_POINT: &str = "/run/korpis";
/// The socket's name in that directory.
const SOCKET: &str = "identity.sock";

/// The identity sockets of the workloads this node runs.
#[derive(Debug)]
pub struct IdentitySockets {
    root: PathBuf,
    credentials: Arc<Credentials>,
    control_plane: Arc<ControlPlane>,
    served: Mutex<HashMap<String, JoinHandle<()>>>,
    /// The relays of the API served to installed extensions' workloads (§9.5 of
    /// docs/08-identity.md).
    relays: Mutex<HashMap<String, JoinHandle<()>>>,
}

/// Where a workload's credential is used, which the socket tells it with the credential: the
/// node's own way to the control plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlane {
    /// The control plane's address, as this node reaches it.
    pub url: String,
    /// The CA its certificate is verified under, PEM, as pinned at enrollment.
    pub ca_pem: Vec<u8>,
}

/// One workload's socket, as a service.
struct Socket {
    workload: String,
    credentials: Arc<Credentials>,
    control_plane: Arc<ControlPlane>,
}

#[expect(refining_impl_trait, reason = "async fn handlers for the generated trait")]
impl agent::WorkloadIdentityService for Socket {
    async fn get_credential(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, agent::GetCredentialRequest>,
    ) -> ServiceResult<agent::GetCredentialResponse> {
        match self.credentials.request(&self.workload).await {
            Ok(c) => Response::ok(agent::GetCredentialResponse {
                credential: c.into(),
                control_plane: self.control_plane.url.clone(),
                ca_pem: self.control_plane.ca_pem.clone(),
                ..Default::default()
            }),
            Err(NoCredential::Refused(reason)) => Err(ConnectError::permission_denied(reason)),
            Err(e @ NoCredential::Unavailable) => Err(ConnectError::unavailable(e.to_string())),
        }
    }
}

/// Whether `workload` can name a directory: a wire identifier, nothing else.
fn plain(workload: &str) -> bool {
    !workload.is_empty()
        && workload.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl IdentitySockets {
    /// Sockets under `root`, forwarding through `credentials`.
    #[must_use]
    pub fn new(
        root: PathBuf,
        credentials: Arc<Credentials>,
        control_plane: ControlPlane,
    ) -> Arc<Self> {
        let control_plane = Arc::new(control_plane);
        let (served, relays) = (Mutex::new(HashMap::new()), Mutex::new(HashMap::new()));
        Arc::new(Self { root, credentials, control_plane, served, relays })
    }

    /// The directory holding `workload`'s socket, which the driver mounts.
    #[must_use]
    pub fn dir(&self, workload: &str) -> PathBuf {
        self.root.join(workload)
    }

    /// Serves `workload`'s socket if it is not served yet, and returns its directory. A socket
    /// left by a previous run is replaced, so a workload adopted after a restart keeps a socket
    /// at the path its container already mounts.
    ///
    /// # Errors
    /// The workload identifier cannot name a directory, or the socket cannot be bound.
    pub fn ensure(&self, workload: &str) -> std::io::Result<PathBuf> {
        if !plain(workload) {
            return Err(std::io::Error::other(format!("{workload:?} is not a workload")));
        }
        let dir = self.dir(workload);
        let mut served = self.served.lock().unwrap_or_else(PoisonError::into_inner);
        if served.get(workload).is_some_and(|t| !t.is_finished()) {
            return Ok(dir);
        }
        std::fs::create_dir_all(&dir)?;
        // The root is the boundary on the host: its owner and group (the agent's, which the
        // driver shares to read the content store) reach the sockets, and no one else does.
        // Inside the workload, the mount shows only its own directory.
        std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o750))?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
        let listener = bind(&dir.join(SOCKET))?;
        let socket = Socket {
            workload: workload.to_owned(),
            credentials: self.credentials.clone(),
            control_plane: self.control_plane.clone(),
        };
        served.insert(workload.to_owned(), tokio::spawn(serve(listener, socket)));
        Ok(dir)
    }

    /// Serves `workload` the control plane's API on [`super::RELAY`] in its directory if
    /// `wanted`, as for an installed extension's, and withdraws it otherwise. Its directory is
    /// served first.
    ///
    /// # Errors
    /// The directory or the socket cannot be made, or the node's address of the API is not one.
    pub fn relay(&self, workload: &str, wanted: bool) -> std::io::Result<()> {
        let mut relays = self.relays.lock().unwrap_or_else(PoisonError::into_inner);
        if !wanted {
            if let Some(task) = relays.remove(workload) {
                task.abort();
                let _ = std::fs::remove_file(self.dir(workload).join(super::RELAY));
            }
            return Ok(());
        }
        if relays.get(workload).is_some_and(|t| !t.is_finished()) {
            return Ok(());
        }
        let dir = self.ensure(workload)?;
        let api = crate::network::api_of(&self.control_plane.url).ok_or_else(|| {
            std::io::Error::other(format!("{:?} names no API", self.control_plane.url))
        })?;
        relays.insert(workload.to_owned(), super::relay::serve(&dir, api)?);
        Ok(())
    }

    /// Stops serving `workload`'s socket and relay and removes its directory.
    pub fn remove(&self, workload: &str) {
        let task = self.served.lock().unwrap_or_else(PoisonError::into_inner).remove(workload);
        if let Some(task) = task {
            task.abort();
        }
        let relay = self.relays.lock().unwrap_or_else(PoisonError::into_inner).remove(workload);
        if let Some(task) = relay {
            task.abort();
        }
        if plain(workload)
            && let Err(e) = std::fs::remove_dir_all(self.dir(workload))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(workload, error = %e, "an identity socket could not be removed");
        }
    }
}

/// Binds `path`, replacing a socket a previous run left; anyone the mount reaches may connect,
/// whatever user the workload's processes are mapped to, and the mount reaches one workload.
pub(super) fn bind(path: &Path) -> std::io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

async fn serve(listener: UnixListener, socket: Socket) {
    let service = ConnectRpcService::new(Arc::new(socket).register(Router::new()));
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(connection(service.clone(), stream));
            }
            Err(e) => {
                tracing::warn!(error = %e, "an identity socket stopped accepting");
                return;
            }
        }
    }
}

async fn connection(service: ConnectRpcService, stream: UnixStream) {
    let hyper = hyper::service::service_fn(move |request| {
        let mut service = service.clone();
        async move { service.call(request).await }
    });
    let served = auto::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), hyper)
        .await;
    if let Err(e) = served {
        tracing::debug!(error = %e, "an identity connection ended");
    }
}
