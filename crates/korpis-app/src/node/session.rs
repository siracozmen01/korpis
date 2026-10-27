// SPDX-License-Identifier: AGPL-3.0-or-later

//! Agent sessions (§4.2 and §7 of docs/02-architecture.md). A session opens with a Hello the
//! node signs with the key it enrolled; the control plane checks the signature, the timestamp
//! against a declared skew window, and the nonce against those it has seen inside the window,
//! then records what the agent reports and that the node is connected. A node is heard through
//! its heartbeats, and one that misses three is unreachable.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    AgentVersion, Clock, EffectWrite as _, NodeFacts, RecipeSource, SessionWrite, Store, Tx,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::node::identity::{hello_message, verify};
use korpis_core::node::{Node, NodeAdmin};
use korpis_core::principal::{Principal, SystemActor};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// The agent protocols this control plane speaks, newest first (§7 of
/// docs/02-architecture.md).
pub const AGENT_PROTOCOLS: &[&str] = &["korpis.agent.v1"];

/// The form a proof signs: [`hello_message`] or
/// [`data_hello_message`](korpis_core::node::identity::data_hello_message).
pub(crate) type ProofMessage<'a> = &'a (dyn Fn(&str, i64, u32, &[u8]) -> Vec<u8> + Sync);

/// The shortest nonce accepted: 128 bits makes a collision across a skew window negligible.
const MIN_NONCE: usize = 16;

/// The declared intervals of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTerms {
    /// How often an agent sends its heartbeat; three missed make the node unreachable.
    pub heartbeat_interval: Duration,
    /// How far a Hello's timestamp may be from the control plane's clock, either way.
    pub auth_skew: Duration,
    /// How long a lease lasts from the holder's request (§4.5 of docs/02-architecture.md).
    pub lease_term: Duration,
    /// The declared bound on clock-rate error, in parts per million: the holder treats its
    /// lease as ending `term × ρ` early.
    pub rho_ppm: u32,
    /// The metering interval every node samples at (§2 of docs/15-observability.md).
    pub metering_interval: Duration,
}

impl Default for SessionTerms {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(30),
            auth_skew: Duration::from_secs(300),
            lease_term: Duration::from_secs(60),
            rho_ppm: 1000,
            metering_interval: Duration::from_secs(3600),
        }
    }
}

impl SessionTerms {
    /// How long a node may go unheard before it is unreachable: three heartbeats.
    #[must_use]
    pub fn staleness(&self) -> Duration {
        self.heartbeat_interval.saturating_mul(3)
    }
}

/// The node's proof that it holds its key (`NodeAuth`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeProof {
    /// The node identifier exactly as the Hello carried it: what was signed.
    pub signed_node: String,
    /// The timestamp's Unix seconds.
    pub seconds: i64,
    /// Its nanoseconds.
    pub nanos: u32,
    /// The nonce.
    pub nonce: Vec<u8>,
    /// Ed25519 over [`hello_message`].
    pub signature: Vec<u8>,
}

/// A Hello, translated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    /// The node.
    pub node: Uuid,
    /// Its proof.
    pub proof: NodeProof,
    /// The agent's release.
    pub agent_version: String,
    /// The protocol it speaks.
    pub protocol: String,
    /// What it reports about the node; its `agent` is filled from the two fields above.
    pub facts: NodeFacts,
}

/// An opened session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The node, as it now stands.
    pub node: Node,
    /// The protocol the session speaks.
    pub protocol: String,
    /// Its declared intervals.
    pub terms: SessionTerms,
}

fn signed_at(proof: &NodeProof) -> Option<SystemTime> {
    let seconds = u64::try_from(proof.seconds).ok()?;
    let at = UNIX_EPOCH.checked_add(Duration::from_secs(seconds))?;
    at.checked_add(Duration::from_nanos(u64::from(proof.nanos)))
}

fn within(at: SystemTime, now: SystemTime, skew: Duration) -> bool {
    match at.duration_since(now) {
        Ok(ahead) => ahead <= skew,
        Err(behind) => behind.duration() <= skew,
    }
}

/// Whether `proof` was made within `skew` of `now`, with a nonce long enough to remember.
pub(crate) fn fresh(
    proof: &NodeProof,
    (now, skew): (SystemTime, Duration),
) -> Result<(), AppError> {
    if !signed_at(proof).is_some_and(|at| within(at, now, skew)) {
        return Err(AppError::invalid("hello.auth.timestamp", "outside the skew window"));
    }
    if proof.nonce.len() < MIN_NONCE {
        return Err(AppError::invalid("hello.auth.nonce", "at least 16 bytes"));
    }
    Ok(())
}

/// The version of an agent protocol named `korpis.agent.v<n>`.
fn version(protocol: &str) -> Option<u32> {
    protocol.strip_prefix("korpis.agent.v")?.parse().ok()
}

/// What a refusal of `theirs` tells the operator: which side to upgrade (§4 of
/// docs/18-operations.md). The window covers every protocol a supported release ships (§4 of
/// docs/10-api.md), so an agent newer than it means the control plane was not upgraded first, and
/// one older than it is an agent from a release no longer supported.
fn unsupported(theirs: &str) -> String {
    let ours = AGENT_PROTOCOLS.join(", ");
    let window: Vec<u32> = AGENT_PROTOCOLS.iter().filter_map(|p| version(p)).collect();
    match (version(theirs), window.iter().min(), window.iter().max()) {
        (Some(v), _, Some(&newest)) if v > newest => format!(
            "this agent speaks {theirs}, newer than this control plane, which speaks {ours}: \
             upgrade the control plane first, then the agents (§4 of docs/18-operations.md)"
        ),
        (Some(v), Some(&oldest), _) if v < oldest => format!(
            "this agent speaks {theirs}, which this control plane no longer speaks ({ours}): \
             upgrade the agent to a supported release (§4 of docs/10-api.md)"
        ),
        _ => format!(
            "this agent speaks {theirs}, which is not a protocol of this control plane: \
                      it speaks {ours}"
        ),
    }
}

/// The store of the block class `class` of `organization`'s nodes (§2.1 of
/// docs/06-storage.md): one storage, which its provider of that name serves.
pub(crate) fn block_store(organization: Uuid, class: &str) -> String {
    format!("block/{organization}/{class}")
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Opens a session for the node a Hello names, if the Hello proves it holds the node's key.
    /// Every refusal of the proof is `Unauthenticated`, except a timestamp outside the window,
    /// which says so: it is the one an honest agent can fix, by its clock.
    pub async fn open_session(&self, mut hello: Hello) -> Result<Session, AppError> {
        if !AGENT_PROTOCOLS.contains(&hello.protocol.as_str()) {
            return Err(AppError::conflict("protocol.unsupported", unsupported(&hello.protocol)));
        }
        let now = self.clock.now();
        let skew = self.session_terms.auth_skew;
        self.check_proof(hello.node, &hello.proof, (now, &hello_message)).await?;
        hello.facts.agent = Some(AgentVersion {
            release: hello.agent_version.clone(),
            protocol: hello.protocol.clone(),
        });
        let forget = now.checked_sub(skew.saturating_mul(2)).unwrap_or(UNIX_EPOCH);
        let mut tx = self.store.begin().await?;
        if !tx.remember_nonce(hello.node, &hello.proof.nonce, now, forget).await? {
            return Err(AppError::Unauthenticated);
        }
        self.bind_stores(&mut tx, hello.node, &mut hello.facts, now).await?;
        if !tx.record_node_facts(hello.node, &hello.facts, now).await? {
            // Decommissioned: the key no longer opens a session.
            return Err(AppError::Unauthenticated);
        }
        tx.commit().await?;
        let node = self.store.node(hello.node).await?.ok_or(AppError::Unauthenticated)?;
        if node.admin == NodeAdmin::Decommissioned {
            return Err(AppError::Unauthenticated);
        }
        Ok(Session { node, protocol: hello.protocol, terms: self.session_terms })
    }

    /// Binds each shared store `facts` declares to `node`'s organization where no organization
    /// holds it, and strikes from its class a store another organization holds, so that a node's
    /// word about its mount never reaches another organization's volumes (§2.1 of
    /// docs/06-storage.md, Findings 201 and 203 of docs/23-walkthroughs.md); each strike, and
    /// each store bound away from the organization that held it, is an effect. A block class's
    /// store is not the node's to say: it is the organization's storage of that class's name,
    /// which every node of the organization offering the class reaches through its provider.
    async fn bind_stores(
        &self,
        tx: &mut S::Tx,
        node: Uuid,
        facts: &mut NodeFacts,
        now: SystemTime,
    ) -> Result<(), AppError> {
        let Some(classes) = facts.storage_classes.as_mut() else { return Ok(()) };
        if classes.iter().all(|c| c.store.is_none() && c.block.is_none()) {
            return Ok(());
        }
        let organization = self.store.node(node).await?.ok_or(AppError::Unauthenticated)?;
        let organization = organization.organization;
        for class in classes.iter_mut().filter(|c| c.block.is_some()) {
            class.store = Some(block_store(organization, &class.name));
        }
        for class in classes.iter_mut() {
            let Some(store) = class.store.clone() else { continue };
            let (before, owner) = tx.bind_store(&store, (node, organization), now).await?;
            let (kind, outcome, said) = match before {
                _ if owner != organization => {
                    class.store = None;
                    ("node.store_refused", Outcome::Failed, "another organization holds it")
                }
                Some(b) if b != organization => {
                    ("node.store_bound", Outcome::Succeeded, "its organization left nothing in it")
                }
                _ => continue,
            };
            let actor = Principal::System(SystemActor::Reconciler);
            let fields = [
                ("class", class.name.clone().into()),
                ("store", store.into()),
                ("reason", said.into()),
            ];
            let mut e = Effect::new(actor, kind, outcome);
            e = if outcome == Outcome::Failed {
                e.error(details(fields))
            } else {
                e.after(details(fields))
            };
            (e.node, e.organization) = (Some(node), Some(organization));
            tx.append_effect(&e).await?;
        }
        Ok(())
    }

    /// Whether `node` may still hold a session: it exists and is not decommissioned. A session
    /// asks before each set it sends, so one whose node was removed ends at its next wake.
    pub async fn session_admitted(&self, node: Uuid) -> Result<bool, AppError> {
        let found = self.store.node(node).await?;
        Ok(found.is_some_and(|n| n.admin != NodeAdmin::Decommissioned))
    }

    /// Whether `proof` is `node`'s signature over `message` (the Hello's or the `DataHello`'s
    /// form), made within the skew window of `now`. The nonce is the caller's to remember, in
    /// the transaction that acts on the proof.
    pub(super) async fn check_proof(
        &self,
        node: Uuid,
        proof: &NodeProof,
        (now, message): (SystemTime, ProofMessage<'_>),
    ) -> Result<(), AppError> {
        fresh(proof, (now, self.session_terms.auth_skew))?;
        let key = self.store.node_key(node).await?.ok_or(AppError::Unauthenticated)?;
        let message = message(&proof.signed_node, proof.seconds, proof.nanos, &proof.nonce);
        if verify(&key, &message, &proof.signature) {
            Ok(())
        } else {
            Err(AppError::Unauthenticated)
        }
    }

    /// Records a heartbeat: what the node reports now, and that it was heard.
    pub async fn heartbeat(&self, node: Uuid, facts: &NodeFacts) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.record_node_facts(node, facts, self.clock.now()).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Marks unreachable every connected node not heard for three heartbeats; how many.
    pub async fn sweep_silent_nodes(&self) -> Result<u64, AppError> {
        let now = self.clock.now();
        let before = now.checked_sub(self.session_terms.staleness()).unwrap_or(UNIX_EPOCH);
        let mut tx = self.store.begin().await?;
        let n = tx.mark_silent_nodes(before, now).await?;
        tx.commit().await?;
        Ok(n)
    }

    /// At start: nothing is known of a node until it is heard again (§3.4 of
    /// docs/01-model.md), so a node recorded as connected is unknown; how many.
    pub async fn forget_connections(&self) -> Result<u64, AppError> {
        let mut tx = self.store.begin().await?;
        let n = tx.forget_connections().await?;
        tx.commit().await?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_agent_is_told_which_side_to_upgrade() {
        assert!(unsupported("korpis.agent.v9").contains("upgrade the control plane first"));
        assert!(unsupported("korpis.agent.v0").contains("upgrade the agent to a supported"));
        assert!(unsupported("other.v1").contains("is not a protocol of this control plane"));
        assert_eq!(version("korpis.agent.v12"), Some(12));
    }
}
