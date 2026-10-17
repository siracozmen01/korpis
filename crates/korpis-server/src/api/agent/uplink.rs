// SPDX-License-Identifier: AGPL-3.0-or-later

//! What comes up a session after the Hello: heartbeats, acknowledgements, and lease requests and
//! releases (§4.2, §4.5 of docs/02-architecture.md). A lease request is answered on the same
//! stream, echoing its nonce.

use super::credential;
use super::session::facts;
use crate::api::ServerApp;
use crate::api::wire::{Refuse, id, wire_id};
use crate::sessions::Sessions;
use connectrpc::{InboundStream, StreamMessage};
use futures_util::StreamExt as _;
use korpis_app::node::LeaseAnswer;
use korpis_app::ports::Sealed;
use korpis_core::id::IdKind;
use korpis_core::workload::lease::Refusal;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_wire::duration;
use korpis_wire::journal::records_from_pb;
use korpis_wire::observation::observation_from_pb;
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
    /// Set once the session is replaced or forgotten.
    pub(super) ended: tokio::sync::watch::Receiver<bool>,
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

/// The cursor, as the node is told it: it may discard up to `through` after its retention
/// window, and sends everything after it again.
pub(super) fn journal_ack(through: u64) -> agent::ControlMessage {
    let ack = agent::JournalAck { seq: through, ..Default::default() };
    agent::ControlMessage { message: Some(Down::JournalAck(Box::new(ack))), ..Default::default() }
}

impl Uplink {
    /// Handles what the agent sends until the stream ends or another session replaces this one.
    pub(super) async fn run(self, mut requests: InboundStream<agent::AgentMessage>) {
        let mut ended = self.ended.clone();
        loop {
            let next = tokio::select! {
                next = requests.next() => next,
                _ = ended.wait_for(|e| *e) => None,
            };
            let Some(next) = next else { break };
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
                let resources = (s.capacity.as_option(), s.allocatable.as_option());
                match facts(resources, &s.drivers, None, s.advertised.as_option()) {
                    Ok(f) => app.heartbeat(node, &f).await.map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                }
            }
            Some(Up::LeaseRequest(r)) => self.lease(r).await,
            Some(Up::LeaseReleased(r)) => self.released(r).await,
            Some(Up::Journal(j)) => self.journal(j).await,
            Some(Up::Observation(o)) => self.observed(o).await,
            Some(Up::CredentialRequest(r)) => self.credential(r).await,
            Some(Up::JobAccessRequest(r)) => {
                super::job::answer(app, node, (**r).clone(), self.tx.clone());
                Ok(())
            }
            Some(Up::CertificateRequest(r)) => {
                super::certificate::answer(app, node, (**r).clone(), self.tx.clone());
                Ok(())
            }
            Some(Up::StepRequest(r)) => {
                super::step::answer(app, node, (**r).clone(), self.tx.clone());
                Ok(())
            }
            Some(Up::FeasibilityResult(r)) => {
                self.sessions.feasibility_result(node, r);
                Ok(())
            }
            Some(Up::ProbeListening(p)) => {
                self.sessions.probe_listening(&p.probe, p.error.clone());
                Ok(())
            }
            Some(Up::ContentStatus(c)) => {
                app.record_content(node, &c.present).await.map_err(|e| e.to_string())
            }
            Some(Up::Presented(p)) => self.presented(p).await,
            _ => Ok(()),
        };
        if let Err(e) = result {
            tracing::warn!(%node, error = %e, "agent message refused");
        }
    }

    /// The assignments the node holds, presented to a restored control plane that settles.
    async fn presented(&self, p: &agent::Presented) -> Result<(), String> {
        let held: Vec<Sealed> = p
            .assignments
            .iter()
            .map(|a| Sealed {
                body: a.assignment.clone(),
                signature: a.signature.clone(),
                key_id: a.key_id.clone(),
            })
            .collect();
        let shown = self.app.present(self.node, &held).await.map_err(|e| e.to_string())?;
        if shown.refused > 0 {
            tracing::warn!(node = %self.node, refused = shown.refused, "presented assignments refused");
        }
        tracing::info!(node = %self.node, verified = shown.verified, floor = ?shown.raised_to,
                       "presented its assignments");
        Ok(())
    }

    async fn lease(&self, r: &agent::LeaseRequest) -> Result<(), String> {
        let workload =
            id(IdKind::Workload, "workload", &r.workload).refuse().map_err(|e| e.to_string())?;
        let a = self.app.request_lease(self.node, workload).await.map_err(|e| e.to_string())?;
        self.tx.send(answer(r, a)).await.map_err(|e| e.to_string())
    }

    async fn credential(&self, r: &agent::CredentialRequest) -> Result<(), String> {
        let issued = match credential::workload(r) {
            Ok(workload) => self
                .app
                .issue_workload_credential(self.node, workload)
                .await
                .map_err(|e| credential::unavailable(&e)),
            Err(e) => Err(e),
        };
        self.tx.send(credential::answer(r, issued)).await.map_err(|e| e.to_string())
    }

    async fn journal(&self, j: &agent::JournalRecords) -> Result<(), String> {
        let records = records_from_pb(&j.records);
        let through =
            self.app.ingest_journal(self.node, records).await.map_err(|e| e.to_string())?;
        self.tx.send(journal_ack(through)).await.map_err(|e| e.to_string())
    }

    async fn observed(&self, o: &korpis_proto::korpis::v1::Observation) -> Result<(), String> {
        let observation = observation_from_pb(self.node, o).map_err(|e| e.to_string())?;
        self.app.observe(self.node, observation).await.map_err(|e| e.to_string())
    }

    async fn released(&self, r: &agent::LeaseReleased) -> Result<(), String> {
        let workload =
            id(IdKind::Workload, "workload", &r.workload).refuse().map_err(|e| e.to_string())?;
        self.app.release_lease(self.node, workload, r.epoch).await.map_err(|e| e.to_string())?;
        Ok(())
    }
}
