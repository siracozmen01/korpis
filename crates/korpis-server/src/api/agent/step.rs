// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's request to resolve a provider step of an install it runs, and its answer on the same
//! stream (§4 of docs/09-recipes.md). Resolving calls the step provider, which may take a while,
//! so each request is answered from a task of its own and the session goes on meanwhile. The
//! answer always comes, echoing the request.

use crate::api::ServerApp;
use crate::api::wire::id;
use korpis_app::AppError;
use korpis_app::step::install::{InstallAt, StepRefusal};
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::step_resolution::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::step::{platform_from_pb, step_result_to_pb};
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

fn refused(reason: &str, retryable: bool) -> Outcome {
    let code = if retryable {
        pb::ErrorCode::ERROR_CODE_UNAVAILABLE
    } else {
        pb::ErrorCode::ERROR_CODE_DENIED
    };
    Outcome::Refused(Box::new(pb::Error {
        code: code.into(),
        reason: reason.to_owned(),
        retryable,
        ..Default::default()
    }))
}

/// What a node is told when resolving failed in the control plane itself: it may ask again.
fn failed(e: &AppError, node: Uuid) -> Outcome {
    tracing::warn!(error = %e, %node, "a provider step could not be resolved");
    refused("step.unavailable", true)
}

async fn resolve(app: &ServerApp, node: Uuid, r: &agent::StepRequest) -> Outcome {
    let plan = match id(IdKind::Plan, "plan", &r.plan) {
        Ok(plan) => plan,
        Err(e) => return refused(&e.message, false),
    };
    let at = InstallAt { plan, step: r.step, action: r.action };
    // What a step resolves to may differ per platform: one never guessed.
    let Some(platform) = r.platform.as_option().map(platform_from_pb) else {
        return refused("step.invalid.platform", false);
    };
    match app.resolve_install_step(node, at, platform).await {
        Ok(Ok(result)) => Outcome::Result(Box::new(step_result_to_pb(&result))),
        Ok(Err(refusal @ StepRefusal::ProviderChanged)) => {
            tracing::info!(%node, plan = %at.plan, "a step's pinned provider no longer answers");
            refused(&refusal.reason(), refusal.retryable())
        }
        Ok(Err(refusal)) => refused(&refusal.reason(), refusal.retryable()),
        Err(e) => failed(&e, node),
    }
}

/// Answers `r` from `node` on `tx`, from a task of its own.
pub(super) fn answer(
    app: &Arc<ServerApp>,
    node: Uuid,
    r: agent::StepRequest,
    tx: Sender<agent::ControlMessage>,
) {
    let app = Arc::clone(app);
    tokio::spawn(async move {
        let outcome = resolve(&app, node, &r).await;
        let answer = agent::StepResolution {
            request: r.request,
            outcome: Some(outcome),
            ..Default::default()
        };
        let message = agent::ControlMessage {
            message: Some(Down::StepResolution(Box::new(answer))),
            ..Default::default()
        };
        if tx.send(message).await.is_err() {
            tracing::debug!(%node, "the session ended before a step was resolved");
        }
    });
}
