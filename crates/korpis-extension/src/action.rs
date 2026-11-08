// SPDX-License-Identifier: Apache-2.0

//! Serving an extension's actions, `ext.<name>.<action>`. The control plane authorizes the person
//! invoking one before the extension hears of it, and sends the invocation with a token: a call
//! the extension makes with [`for_invocation`] acts with its own grants, but only on the objects
//! the invoker named and was authorized on, and only until the invocation finishes. What it does
//! on its own initiative it does with [`Connection::client`] and is attributed to it alone.
//! Registered as the `action` provider under the extension's own name; requires the
//! `provider.action` capability.

use crate::identity::{Connection, CredentialSource};
use crate::provider::{Failure, Handler, Outcome, Unsupported};
use crate::{Error, Transport};
use connectrpc::client::ClientConfig;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use std::future::Future;

/// The header an invocation's token travels in.
pub const INVOCATION_HEADER: &str = "korpis-invocation";

/// What an action answers: a line for the person, and data for a client to render, as JSON text
/// or empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    /// A line for the person who invoked it.
    pub message: String,
    /// JSON text, or empty.
    pub data: String,
}

/// An extension's actions.
pub trait Actions: Send + Sync + 'static {
    /// Performs `invoked.action` with `invoked.arguments` for `invoked.invoker`.
    fn invoke(
        &self,
        invoked: pb::InvokeAction,
    ) -> impl Future<Output = Result<Answer, Failure>> + Send;
}

/// Serves [`Actions`] as a provider's [`Handler`].
#[derive(Debug, Clone)]
pub struct Invoker<A>(pub A);

impl<A: Actions> Handler for Invoker<A> {
    async fn call(&self, call: pb::ProviderCall) -> Outcome {
        let Some(Request::InvokeAction(i)) = call.request else {
            return Unsupported.call(call).await;
        };
        match self.0.invoke(*i).await {
            Ok(a) => Outcome::ActionResult(Box::new(pb::ActionResult {
                message: a.message,
                data: a.data,
                ..Default::default()
            })),
            Err(f) => f.into(),
        }
    }
}

/// The transport and configuration for a generated client that acts for whoever invoked
/// `invoked`: the extension's own credential, and the invocation's token beside it.
///
/// # Errors
/// What obtaining the extension's credential failed with.
pub async fn for_invocation<S: CredentialSource>(
    connection: &Connection<S>,
    invoked: &pb::InvokeAction,
) -> Result<(Transport, ClientConfig), Error> {
    let (http, config) = connection.client().await?;
    Ok((http, config.with_default_header(INVOCATION_HEADER, invoked.invocation.clone())))
}
