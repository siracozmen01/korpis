// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipe trust policies, per organization (§7 of docs/09-recipes.md).

use super::StoreResult;
use korpis_core::principal::Principal;
use korpis_core::recipe::trust::TrustPolicy;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// A policy as an organization set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetPolicy {
    /// The organization that set it.
    pub organization: Uuid,
    /// The policy.
    pub policy: TrustPolicy,
    /// Bumped on every change, for compare-and-swap; 1 when first set.
    pub version: u64,
    /// Who set it.
    pub set_by: Principal,
    /// When.
    pub set_at: SystemTime,
}

/// A change to an organization's own policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyChange {
    /// The organization.
    pub organization: Uuid,
    /// Its new policy; `None` removes its own, so its nearest ancestor's governs again.
    pub policy: Option<TrustPolicy>,
    /// The version after the change: one more than before, 1 for the first.
    pub version: u64,
    /// Who changed it.
    pub set_by: Principal,
    /// When.
    pub set_at: SystemTime,
}

/// Reading policies.
pub trait TrustRead {
    /// The policy that governs `organization`: its own, or the nearest ancestor's. `None` if no
    /// organization from it up to its root has one, and then the default governs.
    fn trust_policy(
        &self,
        organization: Uuid,
    ) -> impl Future<Output = StoreResult<Option<SetPolicy>>> + Send;

    /// The version of `organization`'s own policy, kept counting through a removal; zero if it
    /// never set one.
    fn own_trust_version(
        &self,
        organization: Uuid,
    ) -> impl Future<Output = StoreResult<u64>> + Send;
}

/// Setting policies.
pub trait TrustWrite {
    /// Applies `change` if the organization's own version is still `expected`; false if it
    /// moved meanwhile.
    fn set_trust_policy(
        &mut self,
        change: &PolicyChange,
        expected: u64,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
