// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node reports of the state it keeps beside its workloads: stream segments (§4 of
//! docs/14-streams.md), volumes, snapshots, and backups (§4, §5, and §7 of docs/06-storage.md). Each is believed
//! from a node that once held the workload's lease, released or not: a segment is pruned, a
//! volume deleted, and a snapshot taken of a retained volume long after the workload left the
//! node, and the node that keeps them is the one that ran it.

use super::{rejected, system};
use crate::App;
use crate::error::AppError;
use crate::ports::{
    BackupWrite as _, Clock, RecipeSource, SnapshotWrite as _, Store, StreamWrite as _,
    VolumeWrite as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::backup::{BackupStatus, ReportedBackup};
use korpis_core::workload::stream::{Segment, SegmentEvent, StreamKind};
use korpis_core::workload::volume::{ReportedSnapshot, ReportedVolume, VolumeEvent};
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// A segment of a stream sealed, offloaded, or pruned on the node (§4 of docs/14-streams.md).
    pub(super) async fn segment(
        &self,
        tx: &mut S::Tx,
        node: Uuid,
        (workload, kind): (Uuid, StreamKind),
        event: SegmentEvent,
        segment: &Segment,
    ) -> Result<Effect, AppError> {
        // A segment is sealed as its container goes and pruned long after, so the test is that
        // the node once ran the workload, not that it still does.
        if !self.store.ever_leased(workload, node).await? {
            return Ok(rejected("the node never held the workload's lease", event.action()));
        }
        // Where an offloaded segment went is believed only from a node that was given write
        // access to the workload's streams there; any other event says nothing of it.
        let mut segment = segment.clone();
        if event != SegmentEvent::Offloaded {
            segment.repository = None;
        } else if !self.offloaded_to(workload, node, segment.repository).await? {
            let why = "the node was given no access to the workload's streams there";
            return Ok(rejected(why, event.action()));
        }
        let segment = &segment;
        tx.record_segment((workload, kind), event, segment, node).await?;
        let mut e = system(event.action(), Outcome::Succeeded).after(details([
            ("stream", kind.as_str().into()),
            ("first_offset", segment.first_offset.into()),
            ("end_offset", segment.end_offset.into()),
            ("bytes", segment.bytes.into()),
            ("digest", segment.digest.clone().into()),
        ]));
        e.workload = Some(workload);
        e.grant = self.store.governing_grant(workload).await?;
        Ok(e)
    }

    /// Whether `node` may say it offloaded a segment of `workload`'s stream to `repository`.
    async fn offloaded_to(
        &self,
        workload: Uuid,
        node: Uuid,
        repository: Option<Uuid>,
    ) -> Result<bool, AppError> {
        let Some(repository) = repository else { return Ok(false) };
        Ok(self.store.streams_granted((repository, workload), node).await?)
    }

    /// A volume the node ensured or deleted (§7 of docs/06-storage.md). Deletion comes long
    /// after the workload went, so here too the test is that the node once ran it.
    pub(super) async fn volume(
        &self,
        tx: &mut S::Tx,
        node: Uuid,
        v: &ReportedVolume,
    ) -> Result<Effect, AppError> {
        let action = match v.event {
            VolumeEvent::Ensured => "volume.ensured",
            VolumeEvent::Deleted => "volume.deleted",
        };
        if !self.store.ever_leased(v.workload, node).await? {
            return Ok(rejected("the node never held the workload's lease", action));
        }
        tx.record_volume(node, v, self.clock.now()).await?;
        let mut e = system(action, Outcome::Succeeded).after(details([
            ("name", v.name.clone().into()),
            ("kind", v.kind.as_str().into()),
            ("storage_class", v.storage_class.clone().into()),
            ("quota_enforced", v.quota_enforced.into()),
        ]));
        e.workload = Some(v.workload);
        e.grant = self.store.governing_grant(v.workload).await?;
        Ok(e)
    }

    /// A snapshot taken, rolled back to, or deleted on the node (§4 of docs/06-storage.md).
    pub(super) async fn snapshot(
        &self,
        tx: &mut S::Tx,
        node: Uuid,
        s: &ReportedSnapshot,
    ) -> Result<Effect, AppError> {
        let action = s.event.action();
        if !self.store.ever_leased(s.workload, node).await? {
            return Ok(rejected("the node never held the workload's lease", action));
        }
        if !tx.record_snapshot(node, s, self.clock.now()).await? {
            return Ok(rejected("the node holds no such live volume", action));
        }
        let mut e = system(action, Outcome::Succeeded).after(details([
            ("snapshot", s.snapshot.to_string().into()),
            ("name", s.name.clone().into()),
        ]));
        e.workload = Some(s.workload);
        e.grant = self.store.governing_grant(s.workload).await?;
        Ok(e)
    }

    /// A backup job that ended on the node (§5 of docs/06-storage.md): believed only from the
    /// node holding the backup's volume, which once ran the workload, and only once.
    pub(super) async fn backup(
        &self,
        tx: &mut S::Tx,
        node: Uuid,
        r: &ReportedBackup,
    ) -> Result<Effect, AppError> {
        let action = "backup.finished";
        let Some(mut b) = self.store.backup(r.backup).await?.filter(|b| b.workload == r.workload)
        else {
            return Ok(rejected("no such backup of the workload", action));
        };
        let held = self.store.volume(b.volume).await?.is_some_and(|v| v.node == node);
        if !held || !self.store.ever_leased(r.workload, node).await? {
            return Ok(rejected("the node does not hold the backup's volume", action));
        }
        b.status = if r.ok { BackupStatus::Succeeded } else { BackupStatus::Failed };
        (b.consistent, b.stats, b.error) = (r.consistent, r.stats.clone(), r.error.clone());
        b.finished = Some(self.clock.now());
        if !tx.finish_backup(&b).await? {
            return Ok(rejected("the backup had already ended", action));
        }
        let outcome = if r.ok { Outcome::Succeeded } else { Outcome::Failed };
        let mut e = system(action, outcome).after(details([
            ("backup", b.id.to_string().into()),
            ("consistent", b.consistent.into()),
            ("bytes", b.stats.bytes.into()),
            ("bytes_new", b.stats.bytes_new.into()),
            ("stored_bytes", b.stats.stored_bytes.into()),
        ]));
        if let Some(why) = &b.error {
            e = e.error(details([("message", why.clone().into())]));
        }
        e.workload = Some(r.workload);
        e.plan = Some(b.plan);
        e.grant = self.store.governing_grant(r.workload).await?;
        Ok(e)
    }
}
