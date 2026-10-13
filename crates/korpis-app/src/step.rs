// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resolving a recipe step through its step provider (§4 of docs/09-recipes.md). The answer is
//! a set of content-addressed artifacts the node fetches and the workload's worker writes; an
//! answer that names a path outside the volume or content without its hash is the provider's
//! malfunction, recorded as such, and never reaches a node.

use crate::App;
use crate::error::AppError;
use crate::extension::{CallTarget, Called};
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::extension::{CallRequest, Platform, ProviderType};
use korpis_core::value::ValueMap;
use std::time::Duration;
use uuid::Uuid;

/// One step to resolve.
#[derive(Debug, Clone, PartialEq)]
pub struct StepCall {
    /// The workload being installed.
    pub workload: Uuid,
    /// Its arguments, as the recipe gives them.
    pub arguments: ValueMap,
    /// The platform of the node that will write the artifacts.
    pub platform: Platform,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Resolves a step through the provider `target` names (its name is the step kind), waiting
    /// at most `timeout`. Internal: the install that runs the step decides who may.
    pub async fn run_step(
        &self,
        target: &CallTarget,
        step: StepCall,
        timeout: Duration,
    ) -> Result<Called, AppError> {
        if target.kind != ProviderType::Step {
            return Err(AppError::invalid("provider", "a step provider"));
        }
        let request = CallRequest::RunStep {
            step: target.name.clone(),
            arguments: step.arguments,
            workload: step.workload,
            platform: step.platform,
        };
        self.call_provider(target, request, timeout).await
    }
}
