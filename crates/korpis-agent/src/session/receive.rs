// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the session does with each message the control plane sends: an assignment set believed
//! only once every signature verifies, then kept and acknowledged; the answers to what the node
//! asked, handed to whoever asked; and everything else passed on.

use super::{Event, Session, message, shipped};
use crate::held::HeldSet;
use crate::journal::Shipper;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;

impl Session {
    /// Believes and keeps a revocation list, or refuses it.
    fn revocations(&self, list: &agent::RevocationList) {
        match self.revoked.receive(list) {
            Ok((version, entries)) => {
                let _ = self.events.send(Event::Revocations { version, entries });
            }
            // The list held stays; the control plane sends the next one within seconds.
            Err(e) => tracing::error!(error = %e, "revocation list refused: a security event"),
        }
    }

    /// Holds the token keys in force in place of those held, or refuses them.
    fn keys_in_force(&self, keys: &agent::TokenKeys) {
        match self.token_keys.receive(keys) {
            Ok(n) => tracing::info!(keys = n, "token keys in force"),
            // The keys held stay; the next session sends them again.
            Err(e) => tracing::error!(error = %e, "token keys refused"),
        }
    }

    /// Handles one message; what to send in answer.
    pub(super) async fn receive(
        &self,
        m: agent::ControlMessage,
        shipper: &mut Shipper,
    ) -> Vec<agent::AgentMessage> {
        match m.message {
            Some(Down::AssignmentSet(set)) => self.hold(&set).into_iter().collect(),
            Some(Down::InSync(_)) => self.event(Event::InSync),
            Some(Down::JournalAck(ack)) => shipped(shipper.acknowledged(ack.seq).await),
            Some(Down::RevocationList(list)) => {
                self.revocations(&list);
                Vec::new()
            }
            Some(Down::Relays(r)) => {
                self.name_relays(&r);
                Vec::new()
            }
            Some(Down::Peers(p)) => self.event(Event::Peers(*p)),
            Some(Down::Edges(e)) => self.event(Event::Edges(*e)),
            Some(Down::EdgeLeaseGrant(g)) => self.event(Event::EdgeLeaseGrant(*g)),
            Some(Down::EdgeLeaseRefusal(r)) => self.event(Event::EdgeLeaseRefusal(*r)),
            Some(Down::TokenKeys(keys)) => {
                self.keys_in_force(&keys);
                Vec::new()
            }
            Some(Down::ReachabilityProbe(p)) => {
                let forwarded = self.held.borrow().forwarded();
                let me = (self.identity.node.as_str(), &self.identity.key);
                vec![crate::probe::begin(me, *p, &forwarded).await]
            }
            Some(Down::WorkloadCredential(answer)) => {
                self.credentials.answer(*answer);
                Vec::new()
            }
            Some(Down::CertificateKey(answer)) => {
                self.certificates.answer(*answer);
                Vec::new()
            }
            Some(Down::JobAccess(answer)) => {
                self.jobs.answer(*answer);
                Vec::new()
            }
            Some(Down::StepResolution(answer)) => {
                self.steps.answer(*answer);
                Vec::new()
            }
            Some(Down::HealthAnswer(answer)) => {
                self.queries.answer(*answer);
                Vec::new()
            }
            Some(Down::FeasibilityQuery(q)) => {
                self.answer(q);
                Vec::new()
            }
            Some(other) => {
                let _ = self.inbound.send(other).await;
                Vec::new()
            }
            None => Vec::new(),
        }
    }

    /// Passes `e` on to the agent's loops; nothing to send in answer.
    fn event(&self, e: Event) -> Vec<agent::AgentMessage> {
        let _ = self.events.send(e);
        Vec::new()
    }

    /// Keeps one data connection to each relay `r` names, the control plane's own address
    /// standing for its relay role.
    fn name_relays(&self, r: &agent::Relays) {
        let mut wanted = r.urls.clone();
        if r.control_plane {
            wanted.push(self.identity.server.clone());
        }
        wanted.sort();
        wanted.dedup();
        self.relays.send_if_modified(|held| {
            let changed = *held != wanted;
            if changed {
                held.clone_from(&wanted);
            }
            changed
        });
    }

    /// Believes, keeps, and acknowledges a set, or refuses it and keeps the one held.
    fn hold(&self, set: &agent::AssignmentSet) -> Option<agent::AgentMessage> {
        let keys = &self.identity.keys.assignment_keys;
        let verified = match HeldSet::verify(set, keys, &self.identity.node) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "assignment set refused: a security event");
                let _ = self.events.send(Event::Refused(e));
                return None;
            }
        };
        if let Err(e) = verified.save(&self.state.root().join("assignments.binpb")) {
            // Not kept, not acknowledged: the control plane sends it again on reconnect.
            tracing::error!(error = %e, "the assignment set could not be kept");
            return None;
        }
        let digest = verified.digest.clone();
        self.held.send_replace(verified);
        let _ = self.events.send(Event::Held { digest: digest.clone() });
        let ack = agent::Ack { set_digest: digest, ..Default::default() };
        Some(message(Up::Ack(Box::new(ack))))
    }
}
