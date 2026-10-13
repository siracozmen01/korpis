// SPDX-License-Identifier: Apache-2.0

//! Writing a DNS provider (§3.5 of docs/07-networking.md). The control plane holds what each
//! owner's records should be and sends them whole; the provider makes its zones agree, removing
//! records the owner no longer has, and never touching a record that is not Korpis's. Requires
//! the `provider.dns` capability.

use crate::provider::{Handler, Outcome, Unsupported, failure};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use std::future::Future;

/// Why a provider could not apply an owner's records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// `dns.zone_not_found` for a name in no zone it manages, `dns.conflict` for a name held by a
    /// record it does not own, or a reason of its own.
    pub reason: String,
    /// For display.
    pub message: String,
    /// Whether asking again may succeed (its DNS service was unreachable, say).
    pub retryable: bool,
}

/// A DNS provider.
pub trait DnsProvider: Send + Sync + 'static {
    /// Makes `owner`'s records exactly `records`, and says what changed.
    fn apply(
        &self,
        owner: String,
        records: Vec<pb::DnsRecord>,
    ) -> impl Future<Output = Result<pb::RecordsApplied, Failure>> + Send;
}

/// Serves a [`DnsProvider`] as a provider's [`Handler`].
#[derive(Debug, Clone)]
pub struct Dns<P>(pub P);

impl<P: DnsProvider> Handler for Dns<P> {
    async fn call(&self, call: pb::ProviderCall) -> Outcome {
        let Some(Request::ApplyRecords(a)) = call.request else {
            return Unsupported.call(call).await;
        };
        let a = *a;
        match self.0.apply(a.owner, a.records).await {
            Ok(applied) => Outcome::RecordsApplied(Box::new(applied)),
            Err(f) => failure(&f.reason, &f.message, f.retryable),
        }
    }
}
