// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes (§7 of docs/06-storage.md): kept from what nodes report, read through the project
//! they belong to.

use super::StoreResult;
use crate::page::Page;
use korpis_core::workload::volume::{ReportedSnapshot, ReportedVolume, Snapshot, Volume};
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

/// Reading snapshots.
pub trait SnapshotRead {
    /// A snapshot, deleted or not.
    fn snapshot(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Snapshot>>> + Send;

    /// A volume's snapshots, oldest first; deleted ones only when `deleted`.
    fn snapshots(
        &self,
        volume: Uuid,
        deleted: bool,
    ) -> impl Future<Output = StoreResult<Vec<Snapshot>>> + Send;
}

/// Writing snapshots.
pub trait SnapshotWrite {
    /// Records what `node` reports of a snapshot, at `now`: taken is kept on its first report,
    /// in the volume the node's helper knows by that identifier; deleted is marked so; a
    /// rollback changes no snapshot. False if the node holds no such live volume.
    fn record_snapshot(
        &mut self,
        node: Uuid,
        reported: &ReportedSnapshot,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}

/// Writing volumes.
pub trait VolumeWrite {
    /// Attaches the live volume `volume` to `workload`, if nothing else holds it; whether it is
    /// now `workload`'s.
    fn claim_volume(
        &mut self,
        volume: Uuid,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records what `node` reports of a volume, at `now`: a volume ensured is created on first
    /// report, in the project of the workload it is for, and attached to that workload; one
    /// deleted is marked so, never removed, and a console stream's deletion leaves no segment
    /// of the stream held on `node`. A volume of a shared class is known by its `store` and the
    /// helper's identifier, whichever node reports it (§2.1 of docs/06-storage.md), and only as
    /// the reporting workload's own: false, and nothing recorded, for another's.
    fn record_volume(
        &mut self,
        node: Uuid,
        reported: &ReportedVolume,
        store: Option<&str>,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
