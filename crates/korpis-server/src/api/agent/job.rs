// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's request for what a backup or restore job needs, and its answer on the same stream
//! (§5.5 of docs/06-storage.md). Granting access calls the repository's backup target, which may
//! take seconds, so each request is answered from a task of its own and the session goes on
//! meanwhile. The answer always comes, echoing the request.

use crate::api::ServerApp;
use crate::api::wire::{id, timestamp};
use korpis_app::AppError;
use korpis_app::backup::job::{JobGrant, JobRefusal};
use korpis_app::backup::keys::JobKey;
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::object_access::Protocol;
use korpis_wire::backup::access_to_pb;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

fn refused(reason: &str, retryable: bool) -> Box<pb::Error> {
    Box::new(pb::Error {
        code: if retryable {
            pb::ErrorCode::ERROR_CODE_UNAVAILABLE
        } else {
            pb::ErrorCode::ERROR_CODE_DENIED
        }
        .into(),
        reason: reason.to_owned(),
        retryable,
        ..Default::default()
    })
}

fn outcome(granted: Result<JobGrant, JobRefusal>) -> Outcome {
    match granted {
        Ok(g) => {
            let s3 = |a: &korpis_core::extension::ObjectAccess| match access_to_pb(a).protocol {
                Some(Protocol::S3(s)) => Some(*s),
                _ => None,
            };
            let store = s3(&g.access);
            let content_store = g.content.as_ref().and_then(s3);
            let (data_key, sealed_data_key) = match &g.data_key {
                JobKey::Clear(k) => (k.0.to_vec(), Vec::new()),
                JobKey::Sealed(s) => (Vec::new(), s.clone()),
            };
            Outcome::Grant(Box::new(agent::JobGrant {
                data_key,
                sealed_data_key,
                store: store.into(),
                content_store: content_store.into(),
                expires_at: timestamp(g.access.expires_at).into(),
                repository: korpis_wire::wire_id(IdKind::Repository, g.repository),
                workload: korpis_wire::wire_id(IdKind::Workload, g.workload),
                ..Default::default()
            }))
        }
        Err(r @ (JobRefusal::Store(_) | JobRefusal::NoTarget)) => {
            Outcome::Refused(refused(&r.reason(), true))
        }
        Err(r) => Outcome::Refused(refused(&r.reason(), false)),
    }
}

/// What a node is told when granting its job's access failed: a request it must change is
/// refused for good, and anything else may be asked again.
fn failed(e: &AppError, node: Uuid) -> Outcome {
    match e {
        AppError::Invalid { field, .. } => {
            Outcome::Refused(refused(&format!("job.invalid.{field}"), false))
        }
        AppError::Unavailable { what, detail } => {
            tracing::info!(what, detail, %node, "a job's access waits on a provider");
            Outcome::Refused(refused("job.unavailable", true))
        }
        other => {
            tracing::warn!(error = %other, %node, "a job's access could not be granted");
            Outcome::Refused(refused("job.unavailable", true))
        }
    }
}

/// Answers `r` from `node` on `tx`, from a task of its own.
pub(super) fn answer(
    app: &Arc<ServerApp>,
    node: Uuid,
    r: agent::JobAccessRequest,
    tx: Sender<agent::ControlMessage>,
) {
    let app = Arc::clone(app);
    tokio::spawn(async move {
        let asked = if r.offload.is_empty() {
            match id(IdKind::Plan, "plan", &r.plan) {
                Err(e) => Err(e),
                Ok(plan) => Ok(app.grant_job_access(node, (plan, r.step), &r.recipient).await),
            }
        } else {
            match id(IdKind::Workload, "offload", &r.offload) {
                Err(e) => Err(e),
                Ok(workload) => Ok(app.grant_offload_access(node, workload, &r.recipient).await),
            }
        };
        let outcome = match asked {
            Err(e) => Outcome::Refused(refused(&e.message, false)),
            Ok(Ok(granted)) => outcome(granted),
            Ok(Err(e)) => failed(&e, node),
        };
        let answer =
            agent::JobAccess { request: r.request, outcome: Some(outcome), ..Default::default() };
        let message = agent::ControlMessage {
            message: Some(Down::JobAccess(Box::new(answer))),
            ..Default::default()
        };
        if tx.send(message).await.is_err() {
            tracing::debug!(%node, "the session ended before a job's access was answered");
        }
    });
}
