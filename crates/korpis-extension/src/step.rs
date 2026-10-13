// SPDX-License-Identifier: Apache-2.0

//! Writing a recipe step provider (§4 of docs/09-recipes.md). A step resolves into
//! content-addressed artifacts, each a URL and its SHA-256, which the node fetches once and the
//! workload's own worker writes; the provider never touches the volume. It says whether the
//! request named the content exactly (pinned), and what it resolved to. Requires the
//! `provider.step` capability.

pub use crate::provider::Failure;
use crate::provider::{Handler, Outcome, Unsupported};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use std::collections::HashMap;
use std::future::Future;

/// A step provider.
pub trait StepProvider: Send + Sync + 'static {
    /// Resolves the step kind `step` with `arguments` for a node of `platform`.
    fn run(
        &self,
        step: String,
        arguments: HashMap<String, pb::Value>,
        platform: pb::Platform,
    ) -> impl Future<Output = Result<pb::StepResult, Failure>> + Send;
}

/// Serves a [`StepProvider`] as a provider's [`Handler`].
#[derive(Debug, Clone)]
pub struct Step<P>(pub P);

impl<P: StepProvider> Handler for Step<P> {
    async fn call(&self, call: pb::ProviderCall) -> Outcome {
        let Some(Request::RunStep(r)) = call.request else {
            return Unsupported.call(call).await;
        };
        let r = *r;
        let platform = r.platform.into_option().unwrap_or_default();
        let arguments = r.arguments.into_iter().collect();
        match self.0.run(r.step, arguments, platform).await {
            Ok(result) => Outcome::StepResult(Box::new(result)),
            Err(f) => f.into(),
        }
    }
}
