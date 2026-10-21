// SPDX-License-Identifier: AGPL-3.0-or-later

//! The effect log's record (§4 of docs/03-state.md): what was done, by whom, under which grant,
//! and how it turned out. Every action is one, refusals included.

pub mod denial;
pub mod failure;

use crate::object::{Kind, ObjectRef};
use crate::principal::Principal;
use serde_json::{Map, Value};
use std::time::SystemTime;
use uuid::Uuid;

/// How an action turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It happened.
    Succeeded,
    /// It was attempted and failed.
    Failed,
    /// A plan step that did not need doing.
    Skipped,
    /// Undone by a later step.
    RolledBack,
    /// Refused by authorization; nothing was attempted.
    Denied,
}

impl Outcome {
    const ALL: [Self; 5] =
        [Self::Succeeded, Self::Failed, Self::Skipped, Self::RolledBack, Self::Denied];

    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::RolledBack => "rolled_back",
            Self::Denied => "denied",
        }
    }

    /// The outcome a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.as_str() == s)
    }
}

/// Where a node-originated effect sits in that node's journal (§4.10 of
/// docs/02-architecture.md); the pair is unique, which makes ingestion exactly-once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalOrigin {
    /// The node.
    pub node: Uuid,
    /// Its journal position.
    pub seq: u64,
}

/// One entry of the log.
#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    /// Immutable identity.
    pub id: Uuid,
    /// When the control plane ingested it: the only ordering clock. `None` until stored.
    pub at: Option<SystemTime>,
    /// The originating node's reading, informational only.
    pub occurred_at: Option<SystemTime>,
    /// The plan it executes a step of.
    pub plan: Option<Uuid>,
    /// The step.
    pub step: Option<u32>,
    /// The workload it concerns.
    pub workload: Option<Uuid>,
    /// The node it concerns.
    pub node: Option<Uuid>,
    /// What it is about, the object its action was authorized over (§4 of
    /// docs/15-observability.md); unnamed, its workload's or node's.
    pub object: Option<ObjectRef>,
    /// The organization whose subtree holds its object, which the store resolves as it writes
    /// it; `None` before then, and for a node or for nothing.
    pub organization: Option<Uuid>,
    /// Who acted.
    pub actor: Principal,
    /// The client that relayed what the actor presented, when they acted through one (§5.2 of
    /// docs/08-identity.md).
    pub relay: Option<Principal>,
    /// The grant that permitted it; `None` for a refusal and for installation.
    pub grant: Option<Uuid>,
    /// What was done, e.g. `org.create`.
    pub action: String,
    /// How it turned out.
    pub outcome: Outcome,
    /// For failures and refusals: what went wrong.
    pub error: Option<Map<String, Value>>,
    /// The state before, for changes.
    pub before: Option<Map<String, Value>>,
    /// The state after, for changes.
    pub after: Option<Map<String, Value>>,
    /// Coalesced repeats: how many this entry stands for (§10 of docs/10-api.md).
    pub count: u64,
    /// For coalesced repeats: the last one it covers; `occurred_at` is the first.
    pub window_end: Option<SystemTime>,
    /// For node-originated effects.
    pub origin: Option<JournalOrigin>,
}

impl Effect {
    /// A new effect by `actor`, with nothing but its action and outcome set.
    #[must_use]
    pub fn new(actor: Principal, action: impl Into<String>, outcome: Outcome) -> Self {
        Self {
            id: Uuid::now_v7(),
            at: None,
            occurred_at: None,
            plan: None,
            step: None,
            workload: None,
            node: None,
            object: None,
            organization: None,
            actor,
            relay: None,
            grant: None,
            action: action.into(),
            outcome,
            error: None,
            before: None,
            after: None,
            count: 1,
            window_end: None,
            origin: None,
        }
    }

    /// Names the client that relayed what the actor presented, if one did.
    #[must_use]
    pub fn relayed_by(mut self, relay: Option<Principal>) -> Self {
        self.relay = relay;
        self
    }

    /// Names what it is about.
    #[must_use]
    pub const fn about(mut self, object: ObjectRef) -> Self {
        self.object = Some(object);
        self
    }

    /// What it is about: the object it names, or else its workload, or else its node.
    #[must_use]
    pub fn concerns(&self) -> Option<ObjectRef> {
        self.object
            .or_else(|| self.workload.map(|w| ObjectRef::new(Kind::Workload, w)))
            .or_else(|| self.node.map(|n| ObjectRef::new(Kind::Node, n)))
    }

    /// Names the grant that permitted it.
    #[must_use]
    pub fn under(mut self, grant: Uuid) -> Self {
        self.grant = Some(grant);
        self
    }

    /// Records the state after.
    #[must_use]
    pub fn after(mut self, after: Map<String, Value>) -> Self {
        self.after = Some(after);
        self
    }

    /// Records what went wrong.
    #[must_use]
    pub fn error(mut self, error: Map<String, Value>) -> Self {
        self.error = Some(error);
        self
    }
}

/// The families of action that read their effects under a read of their own, beside the read of
/// what the effect is about (§4 of docs/15-observability.md).
const FAMILY_READS: [(&str, &str); 5] = [
    ("grant", "grant.read"),
    ("plan", "plan.read"),
    ("extension", "extension.read"),
    ("backup", "backup.read"),
    ("event", "event.read"),
];

/// What a caller must hold over `object` to read an effect of `action` about it: the read of
/// the action's family where the family has one, and otherwise the read of the object's kind
/// (§4 of docs/15-observability.md).
#[must_use]
pub fn reader(action: &str, object: ObjectRef) -> &'static str {
    let family = action.split('.').next().unwrap_or_default();
    if let Some((_, read)) = FAMILY_READS.iter().find(|(f, _)| *f == family) {
        return read;
    }
    match object.kind {
        Kind::Organization => "org.read",
        Kind::Project => "project.read",
        Kind::Workload | Kind::Stream => "workload.read",
        Kind::Volume => "volume.read",
        Kind::Endpoint => "endpoint.read",
        Kind::Node => "node.read",
        Kind::Repository => "repository.read",
        Kind::Domain => "domain.read",
        Kind::Certificate => "certificate.read",
    }
}

/// `e` without what names a node, for a reader who may not read that node (§8 of
/// docs/08-identity.md): its node, its journal position, and every `node` or `*_node` among its
/// details.
pub fn without_nodes(e: &mut Effect) {
    e.node = None;
    e.origin = None;
    for details in [&mut e.before, &mut e.after, &mut e.error].into_iter().flatten() {
        details.retain(|k, _| k != "node" && !k.ends_with("_node"));
    }
}

/// A JSON object from pairs, for effect details.
#[must_use]
pub fn details<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_effect_is_read_under_its_familys_read_or_else_its_objects() {
        let w = ObjectRef::new(Kind::Workload, Uuid::now_v7());
        let org = ObjectRef::new(Kind::Organization, Uuid::now_v7());
        assert_eq!(reader("grant.issued", org), "grant.read");
        assert_eq!(reader("plan.approve", w), "plan.read");
        assert_eq!(reader("workload.restart", w), "workload.read");
        assert_eq!(reader("org.theme.set", org), "org.read");
        assert_eq!(reader("lease.issue", w), "workload.read", "lease has no read of its own");
    }

    #[test]
    fn a_reader_who_may_not_read_its_node_reads_no_node_of_it() {
        let mut e =
            Effect::new(Principal::Anonymous, "lease.issue", Outcome::Succeeded).after(details([
                ("node", "n".into()),
                ("previous_node", "p".into()),
                ("epoch", 2.into()),
            ]));
        e.node = Some(Uuid::now_v7());
        e.origin = Some(JournalOrigin { node: Uuid::now_v7(), seq: 1 });
        without_nodes(&mut e);
        assert_eq!((e.node, e.origin), (None, None));
        assert_eq!(e.after.unwrap().keys().collect::<Vec<_>>(), ["epoch"]);
    }

    #[test]
    fn outcomes_round_trip_through_their_stored_names() {
        for o in Outcome::ALL {
            assert_eq!(Outcome::parse(o.as_str()), Some(o));
        }
        assert_eq!(Outcome::parse("maybe"), None);
    }
}
