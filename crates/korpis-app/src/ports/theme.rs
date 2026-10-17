// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations' themes (§7 of docs/11-surface-web.md).

use super::StoreResult;
use korpis_core::principal::Principal;
use korpis_core::tenancy::theme::Theme;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// A theme as an organization set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetTheme {
    /// The organization that set it.
    pub organization: Uuid,
    /// The theme.
    pub theme: Theme,
}

/// A change to an organization's own theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeChange {
    /// The organization.
    pub organization: Uuid,
    /// Its new theme; `None` removes its own, so its nearest ancestor's applies again.
    pub theme: Option<Theme>,
    /// The version after the change: one more than before, 1 for the first.
    pub version: u64,
    /// Who changed it.
    pub set_by: Principal,
    /// When.
    pub set_at: SystemTime,
}

/// Reading themes.
pub trait ThemeRead {
    /// The theme that applies to `organization`: its own, or the nearest ancestor's. `None` if no
    /// organization from it up to its root has one, and then the panel's own applies.
    fn theme(
        &self,
        organization: Uuid,
    ) -> impl Future<Output = StoreResult<Option<SetTheme>>> + Send;

    /// The version of `organization`'s own theme, kept counting through a removal; zero if it
    /// never set one.
    fn own_theme_version(
        &self,
        organization: Uuid,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// The installation's root organization: the first organization created with no parent.
    fn installation_root(&self) -> impl Future<Output = StoreResult<Option<Uuid>>> + Send;
}

/// Setting themes.
pub trait ThemeWrite {
    /// Applies `change` if the organization's own version is still `expected`; false if it moved
    /// meanwhile.
    fn set_theme(
        &mut self,
        change: &ThemeChange,
        expected: u64,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
