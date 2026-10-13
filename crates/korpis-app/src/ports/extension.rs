// SPDX-License-Identifier: AGPL-3.0-or-later

//! Extension installations (§4 of docs/16-extensions.md).

use super::StoreResult;
use crate::Page;
use korpis_core::extension::Installation;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading installations.
pub trait ExtensionRead {
    /// One installation, installed or not.
    fn extension(&self, id: Uuid)
    -> impl Future<Output = StoreResult<Option<Installation>>> + Send;

    /// The installation that runs as `workload`, installed or not.
    fn extension_of_workload(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Installation>>> + Send;

    /// An organization's installed extensions, in identifier order after the page's cursor.
    fn extensions(
        &self,
        organization: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Installation>>> + Send;
}

/// Writing installations.
pub trait ExtensionWrite {
    /// A new installation. Another of its name installed in the same place is
    /// [`Unique::ExtensionName`](super::Unique::ExtensionName).
    fn insert_extension(
        &mut self,
        installation: &Installation,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records the uninstallation at `at`; `false` if it was already uninstalled.
    fn uninstall_extension(
        &mut self,
        id: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
