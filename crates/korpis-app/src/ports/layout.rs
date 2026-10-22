// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations' layouts (question 2 of §10 of docs/11-surface-web.md).

use super::StoreResult;
use korpis_core::principal::Principal;
use korpis_core::tenancy::layout::Layout;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// A layout as an organization set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetLayout {
    /// The organization that set it.
    pub organization: Uuid,
    /// The layout.
    pub layout: Layout,
}

/// A change to an organization's own layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutChange {
    /// The organization.
    pub organization: Uuid,
    /// Its new layout; `None` removes its own, so its nearest ancestor's applies again.
    pub layout: Option<Layout>,
    /// The version after the change: one more than before, 1 for the first.
    pub version: u64,
    /// Who changed it.
    pub set_by: Principal,
    /// When.
    pub set_at: SystemTime,
}

/// Reading layouts.
pub trait LayoutRead {
    /// The layout that applies to `organization`: its own, or the nearest ancestor's. `None` if
    /// no organization from it up to its root has one, and then the panel's own landing applies.
    fn layout(
        &self,
        organization: Uuid,
    ) -> impl Future<Output = StoreResult<Option<SetLayout>>> + Send;

    /// The version of `organization`'s own layout, kept counting through a removal; zero if it
    /// never set one.
    fn own_layout_version(
        &self,
        organization: Uuid,
    ) -> impl Future<Output = StoreResult<u64>> + Send;
}

/// Setting layouts.
pub trait LayoutWrite {
    /// Applies `change` if the organization's own version is still `expected`; false if it moved
    /// meanwhile.
    fn set_layout(
        &mut self,
        change: &LayoutChange,
        expected: u64,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
