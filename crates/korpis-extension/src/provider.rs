// SPDX-License-Identifier: Apache-2.0

//! Serving as a provider (§3.3 and §5 of docs/16-extensions.md). The extension dials out, opens
//! a stream, registers one of the `provides` entries its installation recorded, and is called
//! down that stream. The SDK answers the liveness check every provider answers, runs each other
//! call through the handler at most `concurrency` at a time, drops a call the control plane
//! cancels at its deadline, and reconnects with backoff whenever the stream ends, so a replica of
//! the control plane going away costs the provider a reconnection and nothing else.

use crate::error::Error;
use crate::identity::{Connection, CredentialSource};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use korpis_proto::korpis::v1::__buffa::oneof::provider_control::Message as Down;
use korpis_proto::korpis::v1::__buffa::oneof::provider_message::Message as Up;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::AbortHandle;

pub use korpis_proto::korpis::v1::__buffa::oneof::call_result::Outcome;

/// The first wait before reconnecting; it doubles up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(1);

/// The longest wait before reconnecting.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// What a stream registers as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The kind of provider.
    pub kind: pb::ProviderType,
    /// Which of its installation's `provides` entries.
    pub name: String,
    /// The provider's own version; the one registered most recently receives new calls.
    pub version: String,
    /// How many calls it takes at once, 1 to 1024.
    pub concurrency: u32,
    /// For an identity provider: it signs people in, answering `Authorize` (§9.7 of
    /// docs/08-identity.md). Every other kind leaves it false.
    pub sign_in: bool,
    /// For a block storage provider: whether its attach refuses every other node in the
    /// storage itself (`FENCING_STORAGE_ENFORCED`) or cannot (`FENCING_LEASE_ONLY`), which the
    /// control plane takes as its class's (§2.1 of docs/06-storage.md). Every other kind leaves
    /// it unspecified.
    pub fencing: pb::Fencing,
}

/// Answers calls. A liveness check never reaches it: the SDK answers that itself.
pub trait Handler: Send + Sync + 'static {
    /// Answers one call. Its future is dropped if the control plane cancels the call at its
    /// deadline, so what it does must be safe to abandon part way.
    fn call(&self, call: pb::ProviderCall) -> impl Future<Output = Outcome> + Send;
}

/// A handler for a provider whose type has no calls of its own yet: every call is refused.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unsupported;

impl Handler for Unsupported {
    fn call(&self, _call: pb::ProviderCall) -> impl Future<Output = Outcome> + Send {
        std::future::ready(failure(
            "provider.unsupported",
            "this provider answers no such call",
            false,
        ))
    }
}

/// Why a provider could not answer a call, in the terms its type's interface names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// A stable reason, `dns.zone_not_found` say, or one of the provider's own.
    pub reason: String,
    /// For display.
    pub message: String,
    /// Whether asking again may succeed (its upstream service was unreachable, say).
    pub retryable: bool,
}

impl From<Failure> for Outcome {
    fn from(f: Failure) -> Self {
        failure(&f.reason, &f.message, f.retryable)
    }
}

/// A failed call, as a handler answers one.
#[must_use]
pub fn failure(reason: &str, message: &str, retryable: bool) -> Outcome {
    Outcome::Error(Box::new(pb::Error {
        code: pb::ErrorCode::ERROR_CODE_UNAVAILABLE.into(),
        reason: reason.to_owned(),
        message: message.to_owned(),
        retryable,
        ..Default::default()
    }))
}

fn result(call: String, outcome: Outcome) -> pb::ProviderMessage {
    let r = pb::CallResult { call, outcome: Some(outcome), ..Default::default() };
    pb::ProviderMessage { message: Some(Up::Result(Box::new(r))), ..Default::default() }
}

/// How long until `call`'s deadline.
fn left(call: &pb::ProviderCall) -> Duration {
    let deadline = call.deadline.as_option().map_or(SystemTime::now(), |t| {
        let secs = u64::try_from(t.seconds).unwrap_or(0);
        let nanos = u32::try_from(t.nanos).unwrap_or(0);
        SystemTime::UNIX_EPOCH + Duration::new(secs, nanos)
    });
    deadline.duration_since(SystemTime::now()).unwrap_or_default()
}

/// A provider: one registration, served for as long as it runs.
pub struct Provider<S, H> {
    connection: Arc<Connection<S>>,
    registration: Registration,
    handler: Arc<H>,
    registered: watch::Sender<Option<String>>,
}

impl<S, H> std::fmt::Debug for Provider<S, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider").field("registration", &self.registration).finish_non_exhaustive()
    }
}

impl<S: CredentialSource, H: Handler> Provider<S, H> {
    /// A provider registering as `registration`, answering with `handler`.
    #[must_use]
    pub fn new(connection: Arc<Connection<S>>, registration: Registration, handler: H) -> Self {
        let (registered, _) = watch::channel(None);
        Self { connection, registration, handler: Arc::new(handler), registered }
    }

    /// The registration its current stream holds, `prv_…`, or none while it is reconnecting.
    #[must_use]
    pub fn registered(&self) -> watch::Receiver<Option<String>> {
        self.registered.subscribe()
    }

    /// Serves until it is refused: the installation may not register this, or is gone. Every
    /// other end of a stream is followed by a reconnection, after a backoff while it keeps
    /// failing.
    pub async fn run(&self) -> Error {
        let mut wait = FIRST_BACKOFF;
        loop {
            match self.once().await {
                Ok(()) => wait = FIRST_BACKOFF,
                Err(e) if !e.retryable() => return e,
                Err(e) => tracing::warn!(error = %e, "the provider stream broke; reconnecting"),
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(MAX_BACKOFF);
        }
    }

    /// One stream, from registering to its end.
    async fn once(&self) -> Result<(), Error> {
        let (http, config) = self.connection.client().await?;
        let stream = pb::ProviderServiceClient::new(http, config).serve().await?;
        let (mut up, mut down) = stream.into_split();
        let r = &self.registration;
        let register = pb::Register {
            r#type: r.kind.into(),
            name: r.name.clone(),
            version: r.version.clone(),
            concurrency: r.concurrency,
            sign_in: r.sign_in,
            fencing: r.fencing.into(),
            ..Default::default()
        };
        let first = Up::Register(Box::new(register));
        up.send(pb::ProviderMessage { message: Some(first), ..Default::default() }).await?;
        let answer = down.message::<pb::ProviderControl>().await?;
        let answer = answer.ok_or_else(|| Error::Protocol("no answer to Register".into()))?;
        let Some(Down::Registered(registered)) = answer.to_owned_message().message else {
            return Err(Error::Protocol("the answer to Register was not Registered".into()));
        };
        self.registered.send_replace(Some(registered.registration));
        let room = usize::try_from(r.concurrency).unwrap_or(1).saturating_mul(2) + 1;
        let (tx, mut rx) = mpsc::channel::<pb::ProviderMessage>(room);
        // Results go up in the order handlers finish; a send that fails means the stream is
        // gone, which the reading side learns too.
        let writer = tokio::spawn(async move {
            while let Some(m) = rx.recv().await
                && up.send(m).await.is_ok()
            {}
        });
        let mut calls = Calls::new(r.concurrency, tx, Arc::clone(&self.handler));
        let ended = loop {
            match down.message::<pb::ProviderControl>().await {
                Ok(Some(m)) => calls.receive(m.to_owned_message().message),
                Ok(None) => break Ok(()),
                Err(e) => break Err(e.into()),
            }
        };
        self.registered.send_replace(None);
        calls.abandon();
        writer.abort();
        ended
    }
}

/// The calls in flight on one stream.
struct Calls<H> {
    slots: Arc<Semaphore>,
    results: mpsc::Sender<pb::ProviderMessage>,
    handler: Arc<H>,
    running: HashMap<String, AbortHandle>,
}

impl<H: Handler> Calls<H> {
    fn new(concurrency: u32, results: mpsc::Sender<pb::ProviderMessage>, handler: Arc<H>) -> Self {
        let slots = Arc::new(Semaphore::new(usize::try_from(concurrency).unwrap_or(1)));
        Self { slots, results, handler, running: HashMap::new() }
    }

    /// Acts on one message down the stream.
    fn receive(&mut self, message: Option<Down>) {
        self.running.retain(|_, task| !task.is_finished());
        match message {
            Some(Down::Call(call)) => self.start(*call),
            Some(Down::Cancel(cancel)) => {
                if let Some(task) = self.running.remove(&cancel.call) {
                    task.abort();
                }
            }
            Some(Down::Registered(_)) | None => {}
        }
    }

    /// Answers a liveness check at once, and runs anything else through the handler.
    fn start(&mut self, call: pb::ProviderCall) {
        if let Some(Request::Ping(ping)) = &call.request {
            let pong = pb::Pong { nonce: ping.nonce.clone(), ..Default::default() };
            let _ = self.results.try_send(result(call.call, Outcome::Pong(Box::new(pong))));
            return;
        }
        let (slots, results, handler) =
            (Arc::clone(&self.slots), self.results.clone(), Arc::clone(&self.handler));
        let id = call.call.clone();
        let task = tokio::spawn(async move {
            let Ok(_slot) = slots.acquire_owned().await else { return };
            let deadline = left(&call);
            let name = call.call.clone();
            let late = || failure("provider.deadline", "the handler outlived the deadline", true);
            let outcome =
                tokio::time::timeout(deadline, handler.call(call)).await.unwrap_or_else(|_| late());
            let _ = results.send(result(name, outcome)).await;
        });
        self.running.insert(id, task.abort_handle());
    }

    /// Drops every call in flight: the stream they would answer on is gone, and the control plane
    /// has failed them as retryable already.
    fn abandon(&mut self) {
        for task in self.running.values() {
            task.abort();
        }
        self.running.clear();
    }
}
