// SPDX-License-Identifier: AGPL-3.0-or-later

//! Holding a grant as a member of a set (§5.1 and §5.2 of docs/08-identity.md): through the
//! sets an assertion names, at the moment of use, and, for what a member delegated, only while
//! the set's re-verification still confirms them.

use super::binding::{BindingState, IdentityBinding};
use super::evaluate::ChainError;
use super::grant::{Grant, Subject};
use crate::principal::Principal;
use crate::scope::Hierarchy;
use std::time::SystemTime;
use uuid::Uuid;

/// Whether `subject`, asserted a member of `memberships`, holds a grant to `holder`: they are
/// it, or the assertion names them a member of it and of every set its binding requires its
/// members to be in (§5.2 of docs/08-identity.md). Re-verification asks a provider about the
/// set; only the assertion says, at the moment of use, which sets this member is in.
///
/// # Errors
/// [`ChainError::WrongSubject`] if they are neither it nor a member of it, and
/// [`ChainError::MembershipRequired`] if the assertion lacks a set its binding requires.
pub fn holds(
    (subject, memberships): (&Subject, &[Subject]),
    holder: &Subject,
    hierarchy: &impl Hierarchy,
) -> Result<(), ChainError> {
    if holder == subject {
        return Ok(());
    }
    if !memberships.contains(holder) {
        return Err(ChainError::WrongSubject);
    }
    let requires = hierarchy.vouched(&holder.key).map(|v| v.requires).unwrap_or_default();
    let member = |key: &String| memberships.iter().any(|m| &m.key == key);
    if requires.iter().all(member) { Ok(()) } else { Err(ChainError::MembershipRequired) }
}

/// Refuses a grant a member issued through a set once their membership is no longer confirmed:
/// it stands only while the set's binding vouches for the member too, so a member who left the
/// set suspends what they delegated, and everything beneath it (§5.1 of docs/08-identity.md).
pub(crate) fn member_vouched(
    link: &Grant,
    hierarchy: &impl Hierarchy,
    now: SystemTime,
) -> Result<(), ChainError> {
    let Some(via) = &link.via else { return Ok(()) };
    match hierarchy.member_vouched(&via.set.key, &via.member.key) {
        Some(until) if now <= until => Ok(()),
        _ => Err(ChainError::Unvouched(link.id)),
    }
}

/// A member of a bound set who issued a grant through it, and whether their membership was last
/// confirmed (§5.1 of docs/08-identity.md). Recorded when they issue, unverified until the set's
/// next re-verification asks about them, and asked about with the set on its interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRecord {
    /// The set's binding.
    pub binding: Uuid,
    /// The member.
    pub member: Principal,
    /// Whether the provider last said they are in the set.
    pub state: BindingState,
    /// When it last said so.
    pub last_verified_at: Option<SystemTime>,
    /// How many times the record was written, by a re-verification or a sign-in: what an answer
    /// asked about it is recorded against, so an older answer that they are still in the set
    /// never replaces a newer one (Finding 141 of docs/23-walkthroughs.md).
    pub answers: u64,
}

impl MemberRecord {
    /// Until when the membership holds, under `binding`'s interval and grace: never if it is
    /// unverified or was never confirmed.
    #[must_use]
    pub fn verified_until(&self, binding: &IdentityBinding) -> Option<SystemTime> {
        let t = &binding.terms;
        let last = self.last_verified_at.filter(|_| self.state == BindingState::Verified)?;
        last.checked_add(t.reverify_interval + t.grace)
    }
}
