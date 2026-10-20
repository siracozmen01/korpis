// SPDX-License-Identifier: AGPL-3.0-or-later

//! The delegation ceiling (§5.1 of docs/08-identity.md): a structural bound, which no
//! re-verification has to get right, on how long a grant beneath a set subject lasts.

use super::evaluate::ChainError;
use super::grant::Grant;
use crate::object::ObjectRef;
use crate::scope::Hierarchy;
use std::time::{Duration, SystemTime};

/// The delegation ceiling where no organization sets one: seven days (§5.1 of
/// docs/08-identity.md).
pub const DELEGATION_CEILING: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The longest a grant beneath a set subject lasts at `anchor`: the lowest ceiling any
/// organization of its ancestry sets, else [`DELEGATION_CEILING`]. A verifier that does not hold
/// the ancestry takes the default, which fails closed beneath a lowered ceiling only if the
/// default is lower, and refuses early beneath a root that raised it.
#[must_use]
pub fn ceiling_at(hierarchy: &impl Hierarchy, anchor: ObjectRef) -> Duration {
    let set =
        hierarchy.resource(anchor).and_then(|r| r.chain.iter().filter_map(|n| n.ceiling).min());
    set.unwrap_or(DELEGATION_CEILING)
}

/// Ends `chain[i]` at its ceiling when a set subject holds a grant above it on the chain: it
/// stands until it was issued plus the ceiling at its scope, whatever its own expiry says, so a
/// member's delegation outlives their membership by at most the ceiling, and a ceiling lowered
/// later reaches grants already issued (§5.1 of docs/08-identity.md).
pub(crate) fn within_ceiling(
    chain: &[Grant],
    i: usize,
    hierarchy: &impl Hierarchy,
    now: SystemTime,
) -> Result<(), ChainError> {
    let Some(link) = chain.get(i) else { return Ok(()) };
    if !chain.iter().skip(i + 1).any(|g| g.subject.set) {
        return Ok(());
    }
    let until = link.issued_at.checked_add(ceiling_at(hierarchy, link.scope.anchor()));
    if until.is_some_and(|u| now < u) { Ok(()) } else { Err(ChainError::NotInForce(link.id)) }
}
