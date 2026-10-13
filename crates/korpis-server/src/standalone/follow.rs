// SPDX-License-Identifier: AGPL-3.0-or-later

//! Following the feed (§3.1 of docs/02-architecture.md): the relay opens it with a request its
//! identity key signs, trusting the control plane by the authority it pinned when it joined, and
//! takes in every update. A feed that ends, fails, or stays silent for three of its declared
//! heartbeat intervals is opened again, backing off; a feed the control plane refuses to
//! authenticate means the relay was removed, and following stops.

use super::fed::Fed;
use super::{RelayDir, feed::signed_request};
use connectrpc::client::{ClientConfig, ClientTransport, HttpClient, ServerStream};
use korpis_proto::korpis::relay::v1 as relay;
use std::sync::Arc;
use std::time::Duration;

/// The longest wait between attempts to open the feed.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// How long a feed may stay silent before the first terms say how long it may.
const FIRST_SILENCE: Duration = Duration::from_secs(15);

type Feed = ServerStream<
    <HttpClient as ClientTransport>::ResponseBody,
    relay::__buffa::view::FeedUpdateView<'static>,
>;

/// Why following stopped.
#[derive(Debug, thiserror::Error)]
pub enum FollowError {
    /// The control plane does not know this relay any more.
    #[error(
        "the control plane refuses this relay: it was removed, or its identity is not the one that joined"
    )]
    Refused,
    /// The relay directory is not usable.
    #[error("the relay directory: {0}")]
    Dir(String),
}

/// How one feed ended.
enum Ended {
    /// Open another.
    Again(String),
    /// Stop.
    Refused,
}

/// A client of the control plane the relay in `dir` joined, trusting it by the authority the
/// relay pinned.
pub(super) fn control(
    dir: &RelayDir,
) -> Result<relay::RelayControlServiceClient<HttpClient>, String> {
    let read = |name: &str| std::fs::read(dir.file(name)).map_err(|e| format!("{name}: {e}"));
    let server = String::from_utf8(read("server")?).map_err(|e| e.to_string())?;
    let config = korpis_tls::trusting(&read("ca.pem")?).map_err(|e| e.to_string())?;
    let uri = server.trim().parse().map_err(|e: http::uri::InvalidUri| e.to_string())?;
    Ok(relay::RelayControlServiceClient::new(
        HttpClient::with_tls(Arc::new(config)),
        ClientConfig::new(uri),
    ))
}

async fn open(dir: &RelayDir, relay_id: &str) -> Result<Feed, Ended> {
    let client = control(dir).map_err(Ended::Again)?;
    let key = dir.identity().map_err(|e| Ended::Again(e.to_string()))?;
    let request = signed_request(relay_id, &key).map_err(|e| Ended::Again(e.to_string()))?;
    client.feed(request).await.map_err(|e| ended(&e))
}

fn ended(e: &connectrpc::ConnectError) -> Ended {
    if e.code == connectrpc::ErrorCode::Unauthenticated {
        Ended::Refused
    } else {
        Ended::Again(e.to_string())
    }
}

/// Takes in `stream` until it ends; whether anything arrived, and how it ended.
async fn take_in(fed: &Fed, mut stream: Feed) -> (bool, Ended) {
    let mut heard = false;
    loop {
        let silence = fed.terms().map_or(FIRST_SILENCE, |t| t.heartbeat.saturating_mul(3));
        let next = match tokio::time::timeout(silence, stream.message::<relay::FeedUpdate>()).await
        {
            Ok(Ok(Some(m))) => m.to_owned_message(),
            Ok(Ok(None)) => return (heard, Ended::Again("the feed ended".into())),
            Ok(Err(e)) => return (heard, ended(&e)),
            Err(_) => return (heard, Ended::Again(format!("the feed was silent for {silence:?}"))),
        };
        heard = true;
        if let Err(e) = fed.apply(next) {
            // What was held stays; the next update replaces it.
            tracing::error!(error = %e, "a feed update was refused: a security event");
        }
    }
}

/// Follows the feed of the relay kept in `dir` into `fed`, until the control plane refuses it.
pub async fn follow(dir: RelayDir, fed: Arc<Fed>) -> FollowError {
    let relay_id = match dir.joined() {
        Ok(Some(id)) => id,
        Ok(None) => return FollowError::Dir("this relay has not joined".into()),
        Err(e) => return FollowError::Dir(e.to_string()),
    };
    let mut backoff = Duration::from_secs(1);
    loop {
        let (heard, how) = match open(&dir, &relay_id).await {
            Ok(stream) => {
                tracing::info!("feed open");
                take_in(&fed, stream).await
            }
            Err(how) => (false, how),
        };
        if heard {
            backoff = Duration::from_secs(1);
        }
        match how {
            Ended::Refused => return FollowError::Refused,
            Ended::Again(reason) => {
                tracing::warn!(%reason, "feed lost; opening it again in {backoff:?}");
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}
