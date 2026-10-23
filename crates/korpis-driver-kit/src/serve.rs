// SPDX-License-Identifier: AGPL-3.0-or-later

//! A driver's socket. What a driver can do is close to root, containerd's for the `oci` driver and
//! KVM and the jailer's for the `firecracker` one, so who may ask it is decided by the kernel
//! rather than by anything a client says: the socket is a Unix socket only
//! its owner and group may open, and each connection's peer credentials, as the kernel recorded
//! them at `connect`, must name an allowed user, or the connection is closed before a byte of it
//! is read (§6.1 of docs/02-architecture.md). The protocol is gRPC over HTTP/2, the transport
//! the agent's client speaks.

use connectrpc::{ConnectRpcService, Router};
use hyper_util::rt::{TokioExecutor, TokioIo};
use korpis_proto::korpis::driver::v1::{DriverService, DriverServiceExt as _};
use std::collections::BTreeSet;
use std::future::Future;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;
use tower_service::Service as _;

/// The socket's mode: its owner and its group, nobody else.
const MODE: u32 = 0o660;

/// Binds the socket at `path`, replacing one a previous run left behind.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(MODE))?;
    Ok(listener)
}

/// Whether the peer of `stream` is a user in `allowed`.
fn admitted(stream: &UnixStream, allowed: &BTreeSet<u32>) -> bool {
    match stream.peer_cred() {
        Ok(cred) if allowed.contains(&cred.uid()) => true,
        Ok(cred) => {
            tracing::warn!(uid = cred.uid(), pid = ?cred.pid(), "a connection was refused");
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "a connection without credentials was refused");
            false
        }
    }
}

/// Serves one admitted connection until it ends.
async fn connection(service: ConnectRpcService, stream: UnixStream) {
    let hyper = hyper::service::service_fn(move |request| {
        let mut service = service.clone();
        async move { service.call(request).await }
    });
    let served = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), hyper)
        .await;
    if let Err(e) = served {
        tracing::debug!(error = %e, "a connection ended");
    }
}

/// Serves `driver` on `listener` to the users in `allowed` until `shutdown` resolves; the
/// connections still open then are closed.
pub async fn serve<D: DriverService + 'static>(
    listener: UnixListener,
    allowed: BTreeSet<u32>,
    driver: D,
    shutdown: impl Future<Output = ()>,
) {
    let service = ConnectRpcService::new(Arc::new(driver).register(Router::new()));
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        let stream = tokio::select! {
            () = &mut shutdown => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    continue;
                }
            },
        };
        if !admitted(&stream, &allowed) {
            continue;
        }
        connections.spawn(connection(service.clone(), stream));
    }
    connections.shutdown().await;
}

/// Waits for an interrupt or a termination request.
pub async fn shutdown() {
    use tokio::signal::unix::{SignalKind, signal};
    if let Ok(mut term) = signal(SignalKind::terminate()) {
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// The users allowed to connect: those named, or else the driver's own.
pub fn allowed(named: Vec<u32>) -> std::io::Result<BTreeSet<u32>> {
    use std::os::unix::fs::MetadataExt as _;
    if named.is_empty() {
        return Ok(BTreeSet::from([std::fs::metadata("/proc/self")?.uid()]));
    }
    Ok(named.into_iter().collect())
}
