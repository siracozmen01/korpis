// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's request for a workload's credential, and its answer on the same stream (§9.5 of
//! docs/08-identity.md). The answer always comes, echoing the request, so an agent never waits
//! on a request the control plane dropped.

use crate::api::wire::{id, timestamp};
use korpis_app::AppError;
use korpis_app::node::{CredentialRefusal, IssuedCredential};
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::workload_credential::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::principal;
use uuid::Uuid;

/// The workload a request names, or the refusal its malformed identifier earns.
pub(super) fn workload(r: &agent::CredentialRequest) -> Result<Uuid, Box<pb::Error>> {
    id(IdKind::Workload, "workload", &r.workload)
        .map_err(|e| refusal("request.invalid", &e.message))
}

fn refusal(reason: &str, message: &str) -> Box<pb::Error> {
    Box::new(pb::Error {
        code: pb::ErrorCode::ERROR_CODE_DENIED.into(),
        reason: reason.to_owned(),
        message: message.to_owned(),
        ..Default::default()
    })
}

/// The answer to `request`.
pub(super) fn answer(
    request: &agent::CredentialRequest,
    issued: Result<Result<IssuedCredential, CredentialRefusal>, Box<pb::Error>>,
) -> agent::ControlMessage {
    let outcome = match issued {
        Ok(Ok(c)) => Outcome::Credential(Box::new(agent::Credential {
            secret: c.secret.expose().to_owned(),
            expires_at: timestamp(c.expires_at).into(),
            subject: principal::to_pb(&c.subject).into(),
            ..Default::default()
        })),
        Ok(Err(r)) => Outcome::Refused(refusal(
            r.reason(),
            "the workload is not placed on this node under a live lease",
        )),
        Err(e) => Outcome::Refused(e),
    };
    let answer = agent::WorkloadCredential {
        request: request.request.clone(),
        outcome: Some(outcome),
        ..Default::default()
    };
    agent::ControlMessage {
        message: Some(Down::WorkloadCredential(Box::new(answer))),
        ..Default::default()
    }
}

/// A failure of the store as the node is told it: retryable, with no detail of the cause.
pub(super) fn unavailable(e: &AppError) -> Box<pb::Error> {
    tracing::warn!(error = %e, "a workload credential could not be issued");
    Box::new(pb::Error {
        code: pb::ErrorCode::ERROR_CODE_UNAVAILABLE.into(),
        reason: "credential.unavailable".to_owned(),
        retryable: true,
        ..Default::default()
    })
}
