// SPDX-License-Identifier: AGPL-3.0-or-later

//! Issuing a grant (§3.2 of docs/08-identity.md): attenuating one the issuer holds, themself or,
//! through a set the assertion they act under names, as its member, which the child records as
//! `via` so its validity needs the member verified too (§5.1).

use super::evaluate::{ChainError, verify_chain};
use super::grant::{At, Grant, Subject, Via, check_attenuation};
use super::membership::holds;
use super::scope::Hierarchy;
use std::time::SystemTime;

/// Why a grant could not be issued.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IssueError {
    /// The issuer does not hold the parent.
    #[error("the issuer does not hold the parent grant")]
    NotHolder,
    /// The child does not record that its issuer holds the parent as a member of its set, or
    /// records it when they do not.
    #[error("a grant issued through a set records the set and the member who issued it")]
    Via,
    /// The parent's own chain is not valid now.
    #[error("the parent chain is not valid now: {0}")]
    ParentChain(ChainError),
    /// The override was claimed by a request rather than granted by an identity binding.
    #[error("only an operator's identity binding can override the irreversible-action default")]
    OverrideNotFromBinding,
    /// A root grant is created by installation, never issued.
    #[error("a root grant is created at installation, never issued")]
    RootNotIssuable,
    /// The child is not a valid attenuation of the parent.
    #[error(transparent)]
    Attenuation(#[from] super::grant::AttenuationError),
}

/// Issues `child` from the first grant of `parent_chain` on behalf of `issuer`. Issuing a grant to
/// someone *is* attenuating a grant one holds (§3.2 of docs/08-identity.md), so the issuer must be
/// the parent's subject, or a member of it among `memberships` with the child's `via` naming them
/// (§5.1), the parent's whole chain must be valid now, and the child must be a valid
/// attenuation of it, use limits included. The gateway runs this before inserting the child, in
/// the transaction that takes the parent's remaining uses (§6.2).
///
/// `binding_allows_irreversible` is what the subject's identity binding says, read by the caller
/// from the binding an operator declared (§5.2), never from the request: a child carrying
/// `irreversible_override` is refused unless it is a set subject whose binding allows it.
pub fn issue(
    (issuer, memberships): (&Subject, &[Subject]),
    parent_chain: &[Grant],
    child: &Grant,
    binding_allows_irreversible: bool,
    hierarchy: &impl Hierarchy,
    now: SystemTime,
) -> Result<(), IssueError> {
    if child.root {
        return Err(IssueError::RootNotIssuable);
    }
    if child.irreversible_override && !(child.subject.set && binding_allows_irreversible) {
        return Err(IssueError::OverrideNotFromBinding);
    }
    let Some(parent) = parent_chain.first() else { return Err(IssueError::NotHolder) };
    if child.parent != Some(parent.id) {
        return Err(IssueError::NotHolder);
    }
    holds((issuer, memberships), &parent.subject, hierarchy).map_err(|_| IssueError::NotHolder)?;
    if child.via != Via::through(&parent.subject, issuer) {
        return Err(IssueError::Via);
    }
    verify_chain(parent_chain, hierarchy, now).map_err(IssueError::ParentChain)?;
    check_attenuation(parent, child, hierarchy, At::Issue)?;
    Ok(())
}
