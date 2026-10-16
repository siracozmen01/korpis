// SPDX-License-Identifier: AGPL-3.0-or-later

//! Extension installations (§4 of docs/16-extensions.md).

use super::StoreResult;
use crate::Page;
use korpis_core::extension::{Installation, ProviderType};
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

    /// The extension of `name` installed now in `organization`, or in `project` of it.
    fn installed_extension(
        &self,
        organization: Uuid,
        project: Option<Uuid>,
        name: &str,
    ) -> impl Future<Output = StoreResult<Option<Installation>>> + Send;

    /// The installation in `organization` itself that provides `kind` named `name`, if one is
    /// installed: at most one does (§4.1 of docs/16-extensions.md).
    fn providing_installation(
        &self,
        organization: Uuid,
        kind: ProviderType,
        name: &str,
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
    /// One installation, read within the transaction (Finding 71 of docs/23-walkthroughs.md).
    fn extension_within(
        &mut self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Installation>>> + Send;

    /// A new installation. Another of its name installed in the same place is
    /// [`Unique::ExtensionName`](super::Unique::ExtensionName).
    fn insert_extension(
        &mut self,
        installation: &Installation,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records an upgrade: the installation, still installed and still at `from_digest`, takes
    /// `to`'s version, contract version, and artifact digest; `false` if it had changed.
    fn upgrade_extension(
        &mut self,
        from_digest: &str,
        to: &Installation,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records the uninstallation at `at`; `false` if it was already uninstalled.
    fn uninstall_extension(
        &mut self,
        id: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
