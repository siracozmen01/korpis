// SPDX-License-Identifier: Apache-2.0

//! Writing an address router (§3.7 of docs/07-networking.md). The control plane holds which node
//! each address of a cluster pool is to be routed to, and asks the router to make the network
//! route it so, through the node's advertised addresses, or nowhere: announcing it over BGP, or
//! moving a provider's floating address. Each call says the whole of what is wanted, so a repeat
//! changes nothing, and the node holds the address only once the router has answered. Requires
//! the `provider.address_router` capability.

pub use crate::provider::Failure;
use crate::provider::{Handler, Outcome, Unsupported};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use std::future::Future;

/// An address router.
pub trait AddressRouter: Send + Sync + 'static {
    /// Makes the network route `route.address` to `route.node` through `route.next_hops`, or
    /// nowhere where `route.node` is empty, and answers once it does.
    fn route(&self, route: pb::RouteAddress) -> impl Future<Output = Result<(), Failure>> + Send;
}

/// Serves an [`AddressRouter`] as a provider's [`Handler`].
#[derive(Debug, Clone)]
pub struct Router<R>(pub R);

impl<R: AddressRouter> Handler for Router<R> {
    async fn call(&self, call: pb::ProviderCall) -> Outcome {
        let Some(Request::RouteAddress(r)) = call.request else {
            return Unsupported.call(call).await;
        };
        match self.0.route(*r).await {
            Ok(()) => Outcome::AddressRouted(Box::default()),
            Err(f) => f.into(),
        }
    }
}
