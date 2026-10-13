// SPDX-License-Identifier: AGPL-3.0-or-later

//! What comes up a session after the Hello: heartbeats, acknowledgements, and lease requests and
//! releases (§4.2, §4.5 of docs/02-architecture.md). A lease request is answered on the same
//! stream, echoing its nonce.

use super::session::facts;
use crate::api::ServerApp;
use crate::api::wire::{Refuse, id, wire_id};
use crate::sessions::Sessions;
use connectrpc::{InboundStream, StreamMessage};
use futures_util::StreamExt as _;
use korpis_app::node::LeaseAnswer;
use korpis_core::id::IdKind;
use korpis_core::workload::lease::Refusal;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_wire::duration;
use korpis_wire::spec::continuity_to_pb;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

/// One session's uplink.
pub(super) struct Uplink {
    pub(super) app: Arc<ServerApp>,
    pub(super) sessions: Arc<Sessions>,
    pub(super) node: Uuid,
    pub(super) id: u64,
    pub(super) tx: Sender<agent::ControlMessage>,
}

fn answer(request: &agent::LeaseRequest, a: LeaseAnswer) -> agent::ControlMessage {
    let (workload, nonce) = (request.workload.clone(), request.nonce.clone());
    let message = match a {
        LeaseAnswer::Granted { epoch, term, rho_ppm, on_expiry } => {
            Down::LeaseGrant(Box::new(agent::LeaseGrant {
                workload,
                nonce,
                epoch,
                term: duration(term).into(),
                rho_ppm,
                on_expiry: continuity_to_pb(on_expiry).into(),
                ..Default::default()
            }))
        }
        LeaseAnswer::Refused(r) => {
            let (reason, superseded_by) = match r {
                Refusal::Superseded { by } => (
                    agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_SUPERSEDED,
                    wire_id(IdKind::Node, by),
                ),
                Refusal::NotAssigned => {
                    (agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_NOT_ASSIGNED, String::new())
                }
                Refusal::Unavailable => {
                    (agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_UNAVAILABLE, String::new())
                }
            };
            Down::LeaseRefusal(Box::new(agent::LeaseRefusal {
                workload,
                nonce,
                reason: reason.into(),
                superseded_by,
                ..Default::default()
            }))
        }
    };
    agent::ControlMessage { message: Some(message), ..Default::default() }
}

impl Uplink {
    /// Handles what the agent sends until the stream ends or another session replaces this one.
    pub(super) async fn run(self, mut requests: InboundStream<agent::AgentMessage>) {
        while let Some(next) = requests.next().await {
            if !self.sessions.is_current(self.node, self.id) {
                break;
            }
            match next {
                Ok(m) => self.handle(&m).await,
                Err(e) => {
                    tracing::debug!(node = %self.node, error = %e, "agent stream failed");
                    break;
                }
            }
        }
        self.sessions.unregister(self.node, self.id);
        tracing::info!(node = %self.node, "agent session closed");
    }

    async fn handle(&self, m: &StreamMessage<agent::AgentMessage>) {
        let (app, node) = (&self.app, self.node);
        let m = m.to_owned_message();
        let result = match m.message.as_ref() {
            Some(Up::Ack(a)) => {
                app.acknowledge_set(node, &a.set_digest).await.map_err(|e| e.to_string())
            }
            Some(Up::NodeStatus(s)) => {
                match facts(s.capacity.as_option(), s.allocatable.as_option(), &s.drivers, None) {
                    Ok(f) => app.heartbeat(node, &f).await.map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                }
            }
            Some(Up::LeaseRequest(r)) => self.lease(r).await,
            Some(Up::LeaseReleased(r)) => self.released(r).await,
            // Observations, journal records, and feasibility answers arrive with ingestion.
            _ => Ok(()),
        };
        if let Err(e) = result {
            tracing::warn!(%node, error = %e, "agent message refused");
        }
    }

    async fn lease(&self, r: &agent::LeaseRequest) -> Result<(), String> {
        let workload =
            id(IdKind::Workload, "workload", &r.workload).refuse().map_err(|e| e.to_string())?;
        let a = self.app.request_lease(self.node, workload).await.map_err(|e| e.to_string())?;
        self.tx.send(answer(r, a)).await.map_err(|e| e.to_string())
    }

    async fn released(&self, r: &agent::LeaseReleased) -> Result<(), String> {
        let workload =
            id(IdKind::Workload, "workload", &r.workload).refuse().map_err(|e| e.to_string())?;
        self.app.release_lease(self.node, workload, r.epoch).await.map_err(|e| e.to_string())?;
        Ok(())
    }
}
