// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's request for a certificate its proxy serves with, and its answer on the same stream
//! (§3.2 of docs/07-networking.md). Opening the key reads the store and the TLS key alone, but
//! each request is answered from a task of its own like every other request a node makes, so the
//! session never waits on one. The answer always comes, echoing the request.

use crate::api::ServerApp;
use crate::api::wire::id;
use korpis_app::AppError;
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::certificate_key::Outcome;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

fn refused(reason: &str, retryable: bool) -> Outcome {
    Outcome::Refused(Box::new(pb::Error {
        code: if retryable {
            pb::ErrorCode::ERROR_CODE_UNAVAILABLE
        } else {
            pb::ErrorCode::ERROR_CODE_DENIED
        }
        .into(),
        reason: reason.to_owned(),
        retryable,
        ..Default::default()
    }))
}

/// What `node` is told for certificate `certificate`.
async fn outcome(app: &ServerApp, node: Uuid, certificate: &str) -> Outcome {
    let id = match id(IdKind::Certificate, "certificate", certificate) {
        Ok(id) => id,
        Err(e) => return refused(&e.message, false),
    };
    match app.certificate_for_node(node, id).await {
        Ok((c, key)) => Outcome::Certificate(Box::new(agent::ServedCertificate {
            id: certificate.to_owned(),
            revision: c.revision,
            chain: c.chain,
            key: key.as_str().to_owned(),
            ..Default::default()
        })),
        // Not yet issued, or not yet placed here as the node's set says: asked again later.
        Err(AppError::Conflict { reason, .. }) => refused(reason, true),
        Err(other) => {
            tracing::warn!(error = %other, %node, "a certificate could not be served");
            refused("certificate.unavailable", true)
        }
    }
}

/// Answers `r` from `node` on `tx`, from a task of its own.
pub(super) fn answer(
    app: &Arc<ServerApp>,
    node: Uuid,
    r: agent::CertificateRequest,
    tx: Sender<agent::ControlMessage>,
) {
    let app = Arc::clone(app);
    tokio::spawn(async move {
        let outcome = outcome(&app, node, &r.certificate).await;
        let answer = agent::CertificateKey {
            request: r.request,
            outcome: Some(outcome),
            ..Default::default()
        };
        let message = agent::ControlMessage {
            message: Some(Down::CertificateKey(Box::new(answer))),
            ..Default::default()
        };
        if tx.send(message).await.is_err() {
            tracing::debug!(%node, "the session ended before a certificate was answered");
        }
    });
}
