// SPDX-License-Identifier: Apache-2.0

//! Writing a health provider (§5 of docs/04-runtimes.md, §5 of docs/16-extensions.md). A health
//! provider knows a query protocol, a game's status query say, and nothing of where the workload
//! is: the node that runs the workload holds the socket, inside the workload's network namespace,
//! and runs each round of the exchange, and the provider is asked, after each, what to send next
//! or how the workload is. At most four rounds are run before a verdict is due; an answer beyond
//! the bounds of `korpis.v1.HealthSend` is the provider's malfunction. Requires the
//! `provider.health` capability.

pub use crate::provider::Failure;
use crate::provider::{Handler, Outcome, Unsupported};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use std::future::Future;

/// A health provider.
pub trait HealthProvider: Send + Sync + 'static {
    /// The next round of `step`'s exchange, or its verdict, given the rounds run so far: none
    /// for its first step.
    fn step(
        &self,
        step: pb::HealthStep,
    ) -> impl Future<Output = Result<pb::HealthNext, Failure>> + Send;
}

/// Serves a [`HealthProvider`] as a provider's [`Handler`].
#[derive(Debug, Clone)]
pub struct Health<P>(pub P);

impl<P: HealthProvider> Handler for Health<P> {
    async fn call(&self, call: pb::ProviderCall) -> Outcome {
        let Some(Request::HealthStep(step)) = call.request else {
            return Unsupported.call(call).await;
        };
        match self.0.step(*step).await {
            Ok(next) => Outcome::HealthNext(Box::new(next)),
            Err(f) => f.into(),
        }
    }
}

/// The next round: send `data` and read at most `read_at_most` bytes within `wait`.
#[must_use]
pub fn send(data: &[u8], read_at_most: u32, wait: std::time::Duration) -> pb::HealthNext {
    use korpis_proto::korpis::v1::__buffa::oneof::health_next::Next;
    let wait = buffa_types::google::protobuf::Duration {
        seconds: i64::try_from(wait.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(wait.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    };
    let send = pb::HealthSend {
        data: data.to_vec(),
        read_at_most,
        wait: wait.into(),
        ..Default::default()
    };
    pb::HealthNext { next: Some(Next::Send(Box::new(send))), ..Default::default() }
}

/// The verdict: healthy or not, why, and what the workload said of itself.
#[must_use]
pub fn verdict(healthy: bool, reason: &str) -> pb::HealthNext {
    use korpis_proto::korpis::v1::__buffa::oneof::health_next::Next;
    let v = pb::HealthVerdict { healthy, reason: reason.to_owned(), ..Default::default() };
    pb::HealthNext { next: Some(Next::Verdict(Box::new(v))), ..Default::default() }
}
