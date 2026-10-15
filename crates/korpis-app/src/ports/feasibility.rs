// SPDX-License-Identifier: AGPL-3.0-or-later

//! Asking nodes whether their driver would accept an intent, before one is bound (§2.1 of
//! docs/05-scheduling.md): a `FeasibilityQuery` down each node's session, answered with what the
//! driver's `Prepare` says, nothing created. The adapter owns the sessions and asks the nodes at
//! once. A node it cannot send to, such as one whose session another process holds, is not
//! asked, and the check after the bind is what covers it.

use korpis_core::workload::spec::IntentBody;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use uuid::Uuid;

/// What one node said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feasible {
    /// Its driver's `Prepare` accepted the intent.
    Accepted,
    /// It refused, for this reason.
    Refused(String),
    /// It was asked and did not answer in time, or its driver could not be asked: nothing was
    /// decided, so the node is passed over for this bind and asked again by the next.
    Unanswered,
}

/// The answers to one query, by node; a node not asked is not in it.
pub type Answers = Vec<(Uuid, Feasible)>;

/// Asks nodes over their sessions.
pub trait Feasibility: Send + Sync + std::fmt::Debug {
    /// Asks each of `nodes` whether it would accept `body` with the recipe `manifest`, waiting at
    /// most `within` for all of them together.
    fn ask<'a>(
        &'a self,
        nodes: &'a [Uuid],
        body: &'a IntentBody,
        manifest: &'a [u8],
        within: Duration,
    ) -> Pin<Box<dyn Future<Output = Answers> + Send + 'a>>;
}
