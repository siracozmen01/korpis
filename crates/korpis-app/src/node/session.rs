// SPDX-License-Identifier: AGPL-3.0-or-later

//! Agent sessions (§4.2 and §7 of docs/02-architecture.md). A session opens with a Hello the
//! node signs with the key it enrolled; the control plane checks the signature, the timestamp
//! against a declared skew window, and the nonce against those it has seen inside the window,
//! then records what the agent reports and that the node is connected. A node is heard through
//! its heartbeats, and one that misses three is unreachable.

use crate::App;
use crate::error::AppError;
use crate::ports::{AgentVersion, Clock, NodeFacts, RecipeSource, SessionWrite, Store, Tx};
use korpis_core::node::identity::{hello_message, verify};
use korpis_core::node::{Node, NodeAdmin};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// The agent protocols this control plane speaks, newest first (§7 of
/// docs/02-architecture.md).
pub const AGENT_PROTOCOLS: &[&str] = &["korpis.agent.v1"];

/// The form a proof signs: [`hello_message`] or
/// [`data_hello_message`](korpis_core::node::identity::data_hello_message).
pub(super) type ProofMessage = fn(&str, i64, u32, &[u8]) -> Vec<u8>;

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
}

impl Default for SessionTerms {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(30),
            auth_skew: Duration::from_secs(300),
            lease_term: Duration::from_secs(60),
            rho_ppm: 1000,
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

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Opens a session for the node a Hello names, if the Hello proves it holds the node's key.
    /// Every refusal of the proof is `Unauthenticated`, except a timestamp outside the window,
    /// which says so: it is the one an honest agent can fix, by its clock.
    pub async fn open_session(&self, mut hello: Hello) -> Result<Session, AppError> {
        if !AGENT_PROTOCOLS.contains(&hello.protocol.as_str()) {
            let message = format!("this control plane speaks {}", AGENT_PROTOCOLS.join(", "));
            return Err(AppError::conflict("protocol.unsupported", message));
        }
        let now = self.clock.now();
        let skew = self.session_terms.auth_skew;
        self.check_proof(hello.node, &hello.proof, (now, hello_message)).await?;
        hello.facts.agent = Some(AgentVersion {
            release: hello.agent_version.clone(),
            protocol: hello.protocol.clone(),
        });
        let forget = now.checked_sub(skew.saturating_mul(2)).unwrap_or(UNIX_EPOCH);
        let mut tx = self.store.begin().await?;
        if !tx.remember_nonce(hello.node, &hello.proof.nonce, now, forget).await? {
            return Err(AppError::Unauthenticated);
        }
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

    /// Whether `proof` is `node`'s signature over `message` (the Hello's or the `DataHello`'s
    /// form), made within the skew window of `now`. The nonce is the caller's to remember, in
    /// the transaction that acts on the proof.
    pub(super) async fn check_proof(
        &self,
        node: Uuid,
        proof: &NodeProof,
        (now, message): (SystemTime, ProofMessage),
    ) -> Result<(), AppError> {
        let skew = self.session_terms.auth_skew;
        if !signed_at(proof).is_some_and(|at| within(at, now, skew)) {
            return Err(AppError::invalid("hello.auth.timestamp", "outside the skew window"));
        }
        if proof.nonce.len() < MIN_NONCE {
            return Err(AppError::invalid("hello.auth.nonce", "at least 16 bytes"));
        }
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
        let before =
            self.clock.now().checked_sub(self.session_terms.staleness()).unwrap_or(UNIX_EPOCH);
        let mut tx = self.store.begin().await?;
        let n = tx.mark_silent_nodes(before).await?;
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
