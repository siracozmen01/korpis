// SPDX-License-Identifier: AGPL-3.0-or-later

//! The store, served over TLS as the S3 subset a job uses: getting, putting, and listing objects,
//! path-style, in one bucket. Every request is presigned with a grant's credential, and reaches
//! only what its grant does. Connections are bounded, and one that does not finish its handshake
//! in ten seconds is dropped.

mod request;
mod xml;

use crate::store::Store;
use crate::tokens::Tokens;
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

/// How many connections are served at once.
const CONNECTIONS: usize = 256;
/// How long a handshake may take.
const HANDSHAKE: Duration = Duration::from_secs(10);

/// What the server answers from.
#[derive(Debug, Clone)]
pub struct Served {
    /// The objects.
    pub store: Store,
    /// The grants.
    pub tokens: Arc<Tokens>,
}

/// Serves `served` on `listener` with `tls` until the listener fails.
pub async fn serve(listener: TcpListener, tls: Arc<rustls::ServerConfig>, served: Served) {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    let slots = Arc::new(Semaphore::new(CONNECTIONS));
    let served = Arc::new(served);
    loop {
        let Ok(slot) = slots.clone().acquire_owned().await else { return };
        let stream = match listener.accept().await {
            Ok((s, _)) => s,
            Err(e) => {
                tracing::error!(error = %e, "the store stopped accepting");
                return;
            }
        };
        let (acceptor, served) = (acceptor.clone(), served.clone());
        tokio::spawn(async move {
            connection(acceptor, stream, served).await;
            drop(slot);
        });
    }
}

async fn connection(
    acceptor: tokio_rustls::TlsAcceptor,
    stream: tokio::net::TcpStream,
    served: Arc<Served>,
) {
    let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE, acceptor.accept(stream)).await else {
        return;
    };
    let service = hyper::service::service_fn(move |r| {
        let served = served.clone();
        async move { Ok::<_, std::convert::Infallible>(request::answer(&served, r).await) }
    });
    let conn =
        hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls), service);
    if let Err(e) = conn.await {
        tracing::debug!(error = %e, "a connection ended");
    }
}
