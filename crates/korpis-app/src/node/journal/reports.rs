// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node reports of the state it keeps beside its workloads: stream segments (§4 of
//! docs/14-streams.md), volumes, and snapshots (§4 and §7 of docs/06-storage.md). Each is believed
//! from a node that once held the workload's lease, released or not: a segment is pruned, a
//! volume deleted, and a snapshot taken of a retained volume long after the workload left the
//! node, and the node that keeps them is the one that ran it.

use super::{rejected, system};
use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, RecipeSource, SnapshotWrite as _, Store, StreamWrite as _, VolumeWrite as _,
};
use korpis_core::audit::{Effect, Outcome, details};
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
}
