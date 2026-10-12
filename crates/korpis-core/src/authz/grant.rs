// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grants and attenuation (§3.2 of docs/08-identity.md). A child is valid only if every
//! dimension holds against its parent; the same check runs at issue and again at every use.

use super::action::Action;
use super::scope::{Hierarchy, Scope, scope_within};
use ipnet::IpNet;
use std::collections::BTreeSet;
use std::time::SystemTime;
use uuid::Uuid;

/// Anything that can act. `key` is the stable string the store indexes grants by.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Subject {
    pub key: String,
    /// A set: a chat role, an OIDC group. Its members act through it (§5 of
    /// docs/08-identity.md).
    pub set: bool,
}

impl Subject {
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into(), set: false }
    }

    #[must_use]
    pub fn set(key: impl Into<String>) -> Self {
        Self { key: key.into(), set: true }
    }
}

/// Every condition narrows; a child's are at least as restrictive as its parent's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conditions {
    pub expires_at: Option<SystemTime>,
    pub not_before: Option<SystemTime>,
    /// Empty means unrestricted.
    pub source_cidr: Vec<IpNet>,
    pub requires_mfa: bool,
    /// This grant can propose, not apply (§3.6).
    pub requires_approval_by: Option<Scope>,
    /// Redeemed online (§6.2).
    pub max_uses: Option<u64>,
}

/// Whether, how deep, and with which actions a grant may be attenuated (§3.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delegation {
    pub allowed: bool,
    /// `None` is unbounded, which only a root grant created at installation holds.
    pub max_depth: Option<u32>,
    pub actions: BTreeSet<Action>,
}

/// A grant as evaluation sees it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "link_form, revoked, suspended, and irreversible_override are independent properties"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub id: Uuid,
    pub subject: Subject,
    pub actions: BTreeSet<Action>,
    pub scope: Scope,
    pub conditions: Conditions,
    pub parent: Option<Uuid>,
    pub delegation: Delegation,
    pub link_form: bool,
    /// Online redemptions so far, and uses handed to children at issue.
    pub uses: u64,
    pub revoked: bool,
    pub suspended: bool,
    /// For a grant to a set subject: the operator deliberately overrode the default that keeps
    /// irreversible actions out of it (§5.2). Never meaningful for a link-form grant.
    pub irreversible_override: bool,
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
    #[error("the parent's delegation policy does not allow children")]
    DelegationNotAllowed,
    #[error("the parent's delegation depth is exhausted")]
    DepthExhausted,
    #[error("action {0} is not in the parent's actions")]
    ActionNotHeld(Action),
    #[error("action {0} is not delegable under the parent's policy")]
    ActionNotDelegable(Action),
    #[error("the child's scope is not contained in the parent's")]
    ScopeNotContained,
    #[error("the child must expire no later than the parent")]
    ExpiryWider,
    #[error("the child must not become valid before the parent")]
    NotBeforeEarlier,
    #[error("the child's source networks are not within the parent's")]
    NetworkWider,
    #[error("the parent requires MFA and the child does not")]
    MfaWeaker,
    #[error("the child's use limit exceeds the parent's remaining uses")]
    UsesWider,
    #[error("the child's approval requirement is weaker than the parent's")]
    ApprovalWeaker,
    #[error("the child's delegation policy is wider than the parent permits")]
    DelegationWider,
    #[error("a link-form grant can never delegate")]
    LinkFormDelegates,
    #[error("action {0} is irreversible and cannot be held by a link-form or set-subject grant")]
    IrreversibleToBearerOrSet(Action),
}

/// Whether the use-count dimension is part of the check. At issue it is: the child's limit is
/// taken from the parent's remaining uses. At use it is not, because the parent's remaining uses
/// have legitimately fallen by exactly what its children took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    Issue,
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
