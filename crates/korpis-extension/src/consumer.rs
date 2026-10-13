// SPDX-License-Identifier: Apache-2.0

//! Consuming events (§5 of docs/15-observability.md, §5 of docs/16-extensions.md). Installing an
//! extension created the subscriptions its document declares, held by the installation. A
//! consumer follows every one of them, hands each event to the handler, and acknowledges it only
//! once the handler succeeded, so an event the handler failed on, or never saw because the
//! extension stopped, is delivered again. Delivery is at least once: the event's `id` is how a
//! handler recognises one it has already handled.

use crate::error::Error;
use crate::identity::{Connection, CredentialSource};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::subscribe_response::Item;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// The first wait before following again; it doubles up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(1);

/// The longest wait before following again.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Handles events.
pub trait EventHandler: Send + Sync + 'static {
    /// Handles one event; an error leaves it unacknowledged, to be delivered again.
    fn event(&self, event: pb::Event) -> impl Future<Output = Result<(), String>> + Send;

    /// Hears that events were lost to retention before they could be delivered: the consumer was
    /// away longer than the log keeps them. Nothing to acknowledge; delivery goes on after it.
    fn gap(&self, gap: pb::EventGap) -> impl Future<Output = ()> + Send {
        async move {
            tracing::warn!(resumes_at = %gap.resumes_at, reason = %gap.reason, "events were lost");
        }
    }
}

/// A consumer of an installation's subscriptions.
pub struct Consumer<S, H> {
    connection: Arc<Connection<S>>,
    handler: Arc<H>,
}

impl<S, H> std::fmt::Debug for Consumer<S, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Consumer").finish_non_exhaustive()
    }
}

impl<S: CredentialSource, H: EventHandler> Consumer<S, H> {
    /// A consumer handing events to `handler`.
    #[must_use]
    pub fn new(connection: Arc<Connection<S>>, handler: H) -> Self {
        Self { connection, handler: Arc::new(handler) }
    }

    /// The subscriptions the installation holds.
    pub async fn subscriptions(&self) -> Result<Vec<String>, Error> {
        let (http, config) = self.connection.client().await?;
        let client = pb::EventServiceClient::new(http, config);
        let listed = client.list_subscriptions(pb::ListSubscriptionsRequest::default()).await?;
        Ok(listed.into_owned().subscriptions.into_iter().map(|s| s.id).collect())
    }

    /// Follows `subscription` until it is refused or deleted, reconnecting after a backoff
    /// whenever its stream breaks.
    pub async fn run(&self, subscription: &str) -> Error {
        let mut wait = FIRST_BACKOFF;
        loop {
            match self.follow(subscription).await {
                Ok(()) => wait = FIRST_BACKOFF,
                Err(e) if !e.retryable() => return e,
                Err(e) => tracing::warn!(error = %e, subscription, "following again"),
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(MAX_BACKOFF);
        }
    }

    /// Hands one event to the handler and acknowledges it once handled.
    async fn handle(&self, subscription: &str, e: pb::Event) -> Result<(), Error> {
        let id = e.id.clone();
        let refused = |why| Error::Rpc(connectrpc::ConnectError::new(ErrorCode::Aborted, why));
        self.handler.event(e).await.map_err(refused)?;
        // A fresh client: the credential may have been renewed since the stream opened, and an
        // acknowledgement is a call of its own.
        let (http, config) = self.connection.client().await?;
        let ack = pb::AckRequest {
            subscription: subscription.to_owned(),
            through: id,
            ..Default::default()
        };
        pb::EventServiceClient::new(http, config).ack(ack).await?;
        Ok(())
    }

    /// One stream of `subscription`, until it ends or the handler fails.
    async fn follow(&self, subscription: &str) -> Result<(), Error> {
        let (http, config) = self.connection.client().await?;
        let client = pb::EventServiceClient::new(http, config);
        let request =
            pb::SubscribeRequest { subscription: subscription.to_owned(), ..Default::default() };
        let mut stream = client.subscribe(request).await?;
        while let Some(m) = stream.message().await? {
            match m.to_owned_message().item {
                Some(Item::Event(e)) => self.handle(subscription, *e).await?,
                Some(Item::Gap(g)) => self.handler.gap(*g).await,
                None => {}
            }
        }
        Ok(())
    }
}
