// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's question about a query probe it runs, and its answer on the same stream (§5 of
//! docs/04-runtimes.md): the health provider's next round or verdict, or why the probe was not
//! judged this time. Answering calls the provider, so each question is answered from a task of
//! its own and the session goes on meanwhile. The answer always comes, echoing the question.

use crate::api::ServerApp;
use crate::api::wire::id;
use korpis_app::health::{HealthQuery, ProbeRole, Unjudged};
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::health_answer::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::health::{next_to_pb, round_from_pb};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

fn unjudged(reason: &str, retryable: bool) -> Outcome {
    let code = if retryable {
        pb::ErrorCode::ERROR_CODE_UNAVAILABLE
    } else {
        pb::ErrorCode::ERROR_CODE_DENIED
    };
    Outcome::Unjudged(Box::new(pb::Error {
        code: code.into(),
        reason: reason.to_owned(),
        retryable,
        ..Default::default()
    }))
}

/// The question `r` asks, or the reason it asks nothing answerable.
fn query(r: &agent::HealthQuery) -> Result<HealthQuery, String> {
    let workload = id(IdKind::Workload, "workload", &r.workload).map_err(|e| e.message)?;
    let intent = id(IdKind::Intent, "intent", &r.intent).map_err(|e| e.message)?;
    let probe = match r.probe.as_known() {
        Some(agent::ProbeRole::PROBE_ROLE_READINESS) => ProbeRole::Readiness,
        Some(agent::ProbeRole::PROBE_ROLE_LIVENESS) => ProbeRole::Liveness,
        _ => return Err("health.invalid.probe".into()),
    };
    let within = r.within.as_option().ok_or("health.invalid.within")?;
    let within = korpis_wire::duration_from("within", within).map_err(|e| e.message)?;
    let rounds = r.rounds.iter().map(round_from_pb).collect();
    Ok(HealthQuery { workload, intent, probe, rounds, within: within.max(Duration::ZERO) })
}

async fn answered(app: &ServerApp, node: Uuid, r: &agent::HealthQuery) -> Outcome {
    let q = match query(r) {
        Ok(q) => q,
        Err(reason) => return unjudged(&reason, false),
    };
    match app.health_step(node, q).await {
        Ok(Ok(next)) => Outcome::Next(Box::new(next_to_pb(&next))),
        Ok(Err(u @ Unjudged::ProviderChanged)) => {
            tracing::info!(%node, "a query probe's pinned provider no longer answers");
            unjudged(&u.reason(), u.retryable())
        }
        Ok(Err(u)) => unjudged(&u.reason(), u.retryable()),
        Err(e) => {
            tracing::warn!(error = %e, %node, "a query probe's step could not be answered");
            unjudged("health.unavailable", true)
        }
    }
}

/// Answers `r` from `node` on `tx`, from a task of its own.
pub(super) fn answer(
    app: &Arc<ServerApp>,
    node: Uuid,
    r: agent::HealthQuery,
    tx: Sender<agent::ControlMessage>,
) {
    let app = Arc::clone(app);
    tokio::spawn(async move {
        let outcome = answered(&app, node, &r).await;
        let answer = agent::HealthAnswer {
            request: r.request,
            outcome: Some(outcome),
            ..Default::default()
        };
        let message = agent::ControlMessage {
            message: Some(Down::HealthAnswer(Box::new(answer))),
            ..Default::default()
        };
        if tx.send(message).await.is_err() {
            tracing::debug!(%node, "the session ended before a query probe was answered");
        }
    });
}
