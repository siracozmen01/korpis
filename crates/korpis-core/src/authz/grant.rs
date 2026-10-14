// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grants and attenuation (§3.2 of docs/08-identity.md). A child is valid only if every
//! dimension holds against its parent; the same check runs at issue and again at every use.

use super::action::Action;
use super::scope::{Hierarchy, Scope, scope_within};
use ipnet::IpNet;
use std::collections::BTreeSet;
use std::time::SystemTime;
use uuid::Uuid;

pub use crate::principal::Subject;

/// Every condition narrows; a child's are at least as restrictive as its parent's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conditions {
    /// When it stops being valid.
    pub expires_at: Option<SystemTime>,
    /// When it starts being valid.
    pub not_before: Option<SystemTime>,
    /// Empty means unrestricted.
    pub source_cidr: Vec<IpNet>,
    /// Whether every use needs a second factor.
    pub requires_mfa: bool,
    /// This grant can propose, not apply (§3.6).
    pub requires_approval_by: Option<Scope>,
    /// Redeemed online (§6.2).
    pub max_uses: Option<u64>,
    /// Only the records of a workload's stream written within this range may be read under it
    /// (§8 of docs/14-streams.md); a grant carrying one permits nothing but reading the stream.
    pub stream_range: Option<StreamRange>,
}

/// A range of a stream by the node clock that wrote it: records written at or after `from` and
/// before `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamRange {
    /// The first instant included.
    pub from: SystemTime,
    /// The first instant past it.
    pub to: SystemTime,
}

impl StreamRange {
    /// The only action a ranged grant may carry: reading a stream, which is what the range
    /// narrows; any other action would be held whole.
    pub const ACTION: &'static str = "workload.console.read";

    /// Whether `nanos`, a record's time in nanoseconds since the Unix epoch, falls inside it.
    #[must_use]
    pub fn holds(&self, nanos: i64) -> bool {
        let (from, to) = self.nanos();
        from <= nanos && nanos < to
    }

    /// Its bounds in nanoseconds since the Unix epoch, as records state their time.
    #[must_use]
    pub fn nanos(&self) -> (i64, i64) {
        let n = |t: SystemTime| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        };
        (n(self.from), n(self.to))
    }

    /// Whether it lies within `outer`.
    #[must_use]
    pub fn within(&self, outer: &Self) -> bool {
        outer.from <= self.from && self.to <= outer.to
    }
}

/// Whether, how deep, and with which actions a grant may be attenuated (§3.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delegation {
    /// Whether the holder may attenuate it further at all.
    pub allowed: bool,
    /// `None` is unbounded, which only a root grant created at installation holds.
    pub max_depth: Option<u32>,
    /// The actions a child may carry.
    pub actions: BTreeSet<Action>,
}

/// A grant as evaluation sees it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "link_form, revoked, suspended, irreversible_override, and root are independent"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// Immutable identity.
    pub id: Uuid,
    /// Who holds it.
    pub subject: Subject,
    /// What it permits.
    pub actions: BTreeSet<Action>,
    /// Where.
    pub scope: Scope,
    /// Under which conditions.
    pub conditions: Conditions,
    /// The grant it was attenuated from; `None` only for a root grant.
    pub parent: Option<Uuid>,
    /// What the holder may pass on.
    pub delegation: Delegation,
    /// Held by whoever presents its link, rather than by a named subject (§6.1).
    pub link_form: bool,
    /// Online redemptions so far, and uses handed to children at issue.
    pub uses: u64,
    /// Revoked on this grant itself; ancestors are checked separately.
    pub revoked: bool,
    /// Suspended on this grant itself.
    pub suspended: bool,
    /// For a grant to a set subject: the operator deliberately overrode the default that keeps
    /// irreversible actions out of it (§5.2). Never meaningful for a link-form grant. Set by the
    /// control plane from the subject's identity binding, never from a request; [`issue`]
    /// refuses a child that carries it otherwise.
    ///
    /// [`issue`]: super::evaluate::issue
    pub irreversible_override: bool,
    /// A root grant: created at installation or by local root recovery (§9 of
    /// docs/08-identity.md), the only kind of grant with no parent.
    pub root: bool,
}

impl Grant {
    /// Uses left under `max_uses`, or `None` if the grant has no use limit.
    #[must_use]
    pub fn remaining_uses(&self) -> Option<u64> {
        self.conditions.max_uses.map(|m| m.saturating_sub(self.uses))
    }
}

/// Why a child grant is not a valid attenuation of its parent. One variant per row of the table
/// in §3.2, so a refusal names the dimension that failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttenuationError {
    /// The parent does not allow delegation.
    #[error("the parent's delegation policy does not allow children")]
    DelegationNotAllowed,
    /// The parent's delegation depth is used up.
    #[error("the parent's delegation depth is exhausted")]
    DepthExhausted,
    /// The child carries an action the parent does not.
    #[error("action {0} is not in the parent's actions")]
    ActionNotHeld(Action),
    /// The child carries an action the parent may not delegate.
    #[error("action {0} is not delegable under the parent's policy")]
    ActionNotDelegable(Action),
    /// The child's scope is not within the parent's.
    #[error("the child's scope is not contained in the parent's")]
    ScopeNotContained,
    /// The child outlives the parent.
    #[error("the child must expire no later than the parent")]
    ExpiryWider,
    /// The child starts before the parent.
    #[error("the child must not become valid before the parent")]
    NotBeforeEarlier,
    /// The child admits a network the parent does not.
    #[error("the child's source networks are not within the parent's")]
    NetworkWider,
    /// The parent requires a second factor and the child does not.
    #[error("the parent requires MFA and the child does not")]
    MfaWeaker,
    /// The child has more uses than the parent has left.
    #[error("the child's use limit exceeds the parent's remaining uses")]
    UsesWider,
    /// The child's approval scope is narrower than the parent's.
    #[error("the child's approval requirement is weaker than the parent's")]
    ApprovalWeaker,
    /// The child may delegate more than the parent allows.
    #[error("the child's delegation policy is wider than the parent permits")]
    DelegationWider,
    /// A link-form grant would delegate.
    #[error("a link-form grant can never delegate")]
    LinkFormDelegates,
    /// An irreversible action would go to a bearer link or a set (§5.2).
    #[error("action {0} is irreversible and cannot be held by a link-form or set-subject grant")]
    IrreversibleToBearerOrSet(Action),
    /// The child's stream range is missing or reaches outside the parent's.
    #[error("the child's stream range must lie within the parent's")]
    RangeWider,
    /// A stream range is empty, or carried by a grant that does more than read the stream.
    #[error("a stream range is non-empty and narrows only workload.console.read")]
    RangeMisused,
}

/// Whether the use-count dimension is part of the check. At issue it is: the child's limit is
/// taken from the parent's remaining uses. At use it is not, because the parent's remaining uses
/// have legitimately fallen by exactly what its children took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// When the child is issued: uses are compared with what the parent has left.
    Issue,
    /// At every use: the parent's uses have already fallen by what its children took.
    Use,
}

/// Checks every row of §3.2's table, the delegation rules beneath it, and the approval
/// ordering of §3.6, for `child` attenuated from `parent`.
pub fn check_attenuation(
    parent: &Grant,
    child: &Grant,
    hierarchy: &impl Hierarchy,
    at: At,
) -> Result<(), AttenuationError> {
    let pd = &parent.delegation;
    if !pd.allowed || parent.link_form {
        return Err(AttenuationError::DelegationNotAllowed);
    }
    if pd.max_depth == Some(0) {
        return Err(AttenuationError::DepthExhausted);
    }
    for action in &child.actions {
        if !parent.actions.contains(action) {
            return Err(AttenuationError::ActionNotHeld(action.clone()));
        }
        if !pd.actions.contains(action) {
            return Err(AttenuationError::ActionNotDelegable(action.clone()));
        }
    }
    // Grants to set subjects and link-form grants cannot include irreversible actions (§5.2,
    // §6.1). A set's binding may be overridden by an operator, deliberately; a link never.
    let exposed = child.link_form || (child.subject.set && !child.irreversible_override);
    if exposed && let Some(a) = child.actions.iter().find(|a| a.is_irreversible()) {
        return Err(AttenuationError::IrreversibleToBearerOrSet(a.clone()));
    }
    if !scope_within(&child.scope, &parent.scope, hierarchy) {
        return Err(AttenuationError::ScopeNotContained);
    }
    check_conditions(&parent.conditions, &child.conditions, hierarchy)?;
    if let Some(r) = &child.conditions.stream_range
        && (r.from >= r.to || child.actions.iter().any(|a| a.as_str() != StreamRange::ACTION))
    {
        return Err(AttenuationError::RangeMisused);
    }
    if at == At::Issue
        && let Some(remaining) = parent.remaining_uses()
        && child.conditions.max_uses.is_none_or(|m| m > remaining)
    {
        return Err(AttenuationError::UsesWider);
    }
    check_child_delegation(parent, child)
}

fn check_conditions(
    parent: &Conditions,
    child: &Conditions,
    hierarchy: &impl Hierarchy,
) -> Result<(), AttenuationError> {
    if let Some(p) = parent.expires_at
        && child.expires_at.is_none_or(|c| c > p)
    {
        return Err(AttenuationError::ExpiryWider);
    }
    if let Some(p) = parent.not_before
        && child.not_before.is_none_or(|c| c < p)
    {
        return Err(AttenuationError::NotBeforeEarlier);
    }
    if !parent.source_cidr.is_empty()
        && (child.source_cidr.is_empty()
            || !child.source_cidr.iter().all(|c| parent.source_cidr.iter().any(|p| p.contains(c))))
    {
        return Err(AttenuationError::NetworkWider);
    }
    if parent.requires_mfa && !child.requires_mfa {
        return Err(AttenuationError::MfaWeaker);
    }
    if let Some(p) = &parent.stream_range
        && child.stream_range.is_none_or(|c| !c.within(p))
    {
        return Err(AttenuationError::RangeWider);
    }
    // "At least as strict" means equal or larger: approval may move up the tree toward fewer,
    // more senior approvers, never down toward the child's holder (§3.6).
    if let Some(p) = &parent.requires_approval_by {
        match &child.requires_approval_by {
            Some(c) if scope_within(p, c, hierarchy) => {}
            _ => return Err(AttenuationError::ApprovalWeaker),
        }
    }
    Ok(())
}

fn check_child_delegation(parent: &Grant, child: &Grant) -> Result<(), AttenuationError> {
    let (pd, cd) = (&parent.delegation, &child.delegation);
    if child.link_form && cd.allowed {
        return Err(AttenuationError::LinkFormDelegates);
    }
    if !cd.allowed {
        return Ok(());
    }
    // A child that may itself delegate needs the parent to permit at least two more levels.
    if pd.max_depth.is_some_and(|d| d < 2) {
        return Err(AttenuationError::DelegationWider);
    }
    // A bounded parent's child must be bounded, one level shallower.
    if let Some(p) = pd.max_depth
        && cd.max_depth.is_none_or(|c| c > p - 1)
    {
        return Err(AttenuationError::DelegationWider);
    }
    if !cd.actions.iter().all(|a| child.actions.contains(a) && pd.actions.contains(a)) {
        return Err(AttenuationError::DelegationWider);
    }
    Ok(())
}

/// Whether a condition set is in force at `now` on the clock it is evaluated against.
#[must_use]
pub fn in_force(conditions: &Conditions, now: SystemTime) -> bool {
    conditions.expires_at.is_none_or(|e| now < e) && conditions.not_before.is_none_or(|n| now >= n)
}
