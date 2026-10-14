// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes (§7 of docs/06-storage.md): kept from what nodes report, read through the project
//! they belong to.

use super::StoreResult;
use crate::page::Page;
use korpis_core::workload::volume::{ReportedVolume, Volume};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading volumes.
pub trait VolumeRead {
    /// A volume, deleted ones included.
    fn volume(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Volume>>> + Send;

    /// A project's volumes, or only those created for or attached to `workload`, after the
    /// page's cursor; deleted ones only when `deleted`.
    fn volumes(
        &self,
        project: Uuid,
        workload: Option<Uuid>,
        deleted: bool,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Volume>>> + Send;
}

/// Writing volumes.
pub trait VolumeWrite {
    /// Records what `node` reports of a volume, at `now`: a volume ensured is created on first
    /// report, in the project of the workload it is for, and attached to that workload; one
    /// deleted is marked so, never removed.
    fn record_volume(
        &mut self,
        node: Uuid,
        reported: &ReportedVolume,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
