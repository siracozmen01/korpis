// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes through the privileged helper, each one reported (§7 of docs/06-storage.md): the node
//! journals every volume it ensures or deletes, and the control plane keeps the volume object
//! from those records. The record follows the helper's answer, so a volume the journal names was
//! there; one ensured whose record did not reach the journal fails the step or the build that
//! asked for it, and the retry, ensuring it again, reports it.

use super::act::{Failure, volume_id};
use super::step::{After, detail, flag, number};
use super::streams::stream_name;
use super::{Reconciler, text};
use crate::host::{Host, SnapshotOp};
use crate::runtime::Runtime;
use korpis_core::id::IdKind;
use korpis_core::workload::stream::StreamKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::wire_id;
use uuid::Uuid;

/// Why a volume exists, as its report says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Why {
    /// The workload declared it.
    Declared,
    /// It holds the workload's console stream.
    Stream,
}

/// The driver `a`'s target names, empty when it names none.
pub(crate) fn driver_of(a: &agent::Assignment) -> &str {
    let body = a.target.as_option().and_then(|t| t.body.as_option());
    body.and_then(|b| b.runtime.as_option()).map_or("", |r| r.driver.as_str())
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Ensures the volume `request` names, known to its workload as `name`, and reports it.
    pub(super) async fn ensure_volume(
        &self,
        name: &str,
        why: Why,
        request: &helper::EnsureVolumeRequest,
    ) -> Result<helper::EnsureVolumeResponse, Failure> {
        let made = self.host.ensure_volume(request).await?;
        let storage_class = if made.storage_class.is_empty() {
            request.storage_class.clone()
        } else {
            made.storage_class.clone()
        };
        self.report_volume(agent::VolumeEvent {
            workload: request.workload.clone(),
            name: name.to_owned(),
            helper_id: request.volume.clone(),
            storage_class,
            size_bytes: request.size_bytes,
            quota_enforced: made.quota_enforced,
            event: agent::VolumeEventKind::VOLUME_EVENT_KIND_ENSURED.into(),
            stream: why == Why::Stream,
            ..Default::default()
        })
        .await?;
        Ok(made)
    }

    /// Deletes the volume the helper knows as `helper_id`, made for `workload` as `name`, and
    /// reports it. A console stream's is read through first, so the index hears of every
    /// segment before it hears they are gone.
    pub(super) async fn delete_volume(
        &mut self,
        workload: &str,
        (name, why): (&str, Why),
        helper_id: &str,
    ) -> Result<(), Failure> {
        if why == Why::Stream {
            // A run's stream volume is named for its run, and so is the stream it holds.
            let run = StreamKind::of_run_volume(name).map_or("", |(_, run)| run);
            self.finish_stream(&stream_name(workload, run)).await.map_err(|e| {
                Failure(format!("the stream's last changes could not be journaled: {e}"))
            })?;
        }
        self.host.delete_volume(helper_id).await?;
        self.report_volume(agent::VolumeEvent {
            workload: workload.to_owned(),
            name: name.to_owned(),
            helper_id: helper_id.to_owned(),
            event: agent::VolumeEventKind::VOLUME_EVENT_KIND_DELETED.into(),
            stream: why == Why::Stream,
            ..Default::default()
        })
        .await
    }

    /// Executes one of a plan's volume steps; what it leaves behind.
    pub(super) async fn volume_step(
        &mut self,
        a: &agent::Assignment,
        s: &pb::Step,
    ) -> Result<After, Failure> {
        let mut after = After::new();
        let name = detail(s, "volume").unwrap_or_default();
        // The helper's identifier as the node reported it, which the control plane put in the
        // step; never one derived again from the name.
        let reported = || {
            detail(s, "helper_id")
                .ok_or_else(|| Failure("the step names no helper identifier".into()))
        };
        match s.action.as_str() {
            "create_volume" | "attach_volume" => {
                let request = helper::EnsureVolumeRequest {
                    volume: volume_id(a, name),
                    workload: a.workload.clone(),
                    storage_class: detail(s, "storage_class").unwrap_or_default().to_owned(),
                    size_bytes: number(s, "size_bytes"),
                    block_device: self.wiring.driver_kinds.block_device(driver_of(a)),
                    ..Default::default()
                };
                let made = self.ensure_volume(name, Why::Declared, &request).await?;
                after.insert("volume".into(), text(&request.volume));
                after.insert("created".into(), text(if made.created { "true" } else { "false" }));
            }
            "promote_replica" => {
                let made = self.promote(a, name, s).await?;
                after.insert("volume".into(), text(&made));
            }
            // A node-local volume is mounted by the runtime object, so it is detached when the
            // object is next rebuilt or destroyed; its data is retained.
            "detach_volume" => {
                after.insert("applies_at".into(), text("the next rebuild of the runtime object"));
            }
            "delete_volume" => {
                let why = if flag(s, "stream") { Why::Stream } else { Why::Declared };
                self.delete_volume(&a.workload, (name, why), reported()?).await?;
                after.insert("volume".into(), text(reported()?));
            }
            action => {
                let op = match action {
                    "snapshot_volume" => SnapshotOp::Take,
                    "rollback_volume" => SnapshotOp::Rollback,
                    "delete_snapshot" => SnapshotOp::Delete,
                    other => return Err(Failure(format!("not a volume step: {other:?}"))),
                };
                let snapshot = detail(s, "snapshot_id")
                    .ok_or_else(|| Failure("the step names no snapshot".into()))?;
                let named = (detail(s, "snapshot").unwrap_or_default(), snapshot);
                self.on_snapshot(&a.workload, op, reported()?, named).await?;
                after.insert("snapshot".into(), text(snapshot));
            }
        }
        Ok(after)
    }

    /// Makes the volume `name` of `a`'s workload here from the snapshot of its replica here
    /// that step `s` names, under the identifier the step gives it, and reports it ensured with
    /// its limits set, as a volume attached is (§2.2 of docs/06-storage.md); that identifier.
    /// Asked again, the helper answers the copy it already made, so a step retried after its
    /// report failed reports it then.
    async fn promote(
        &self,
        a: &agent::Assignment,
        name: &str,
        s: &pb::Step,
    ) -> Result<String, Failure> {
        let named = |key: &str| {
            detail(s, key)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| Failure(format!("the step names no {key}")))
        };
        let record = Uuid::parse_str(named("existing")?)
            .map_err(|_| Failure("the step names no volume record".into()))?;
        let storage_class = detail(s, "storage_class").unwrap_or_default().to_owned();
        let into = named("into")?.to_owned();
        let promotion = helper::PromoteReplicaRequest {
            volume: wire_id(IdKind::Volume, record),
            storage_class: storage_class.clone(),
            snapshot: named("snapshot")?.to_owned(),
            into: into.clone(),
            ..Default::default()
        };
        self.host.promote_replica(&promotion).await?;
        let request = helper::EnsureVolumeRequest {
            volume: into.clone(),
            workload: a.workload.clone(),
            storage_class,
            size_bytes: number(s, "size_bytes"),
            ..Default::default()
        };
        self.ensure_volume(name, Why::Declared, &request).await?;
        Ok(into)
    }

    /// Does `op` to snapshot `snapshot` (its name, then its identifier) of the volume the helper
    /// knows as `volume`, made for `workload`, and reports it.
    async fn on_snapshot(
        &self,
        workload: &str,
        op: SnapshotOp,
        volume: &str,
        (name, snapshot): (&str, &str),
    ) -> Result<(), Failure> {
        self.host.snapshot(op, volume, snapshot).await?;
        let event = match op {
            SnapshotOp::Take => agent::SnapshotEventKind::SNAPSHOT_EVENT_KIND_TAKEN,
            SnapshotOp::Rollback => agent::SnapshotEventKind::SNAPSHOT_EVENT_KIND_ROLLED_BACK,
            SnapshotOp::Delete => agent::SnapshotEventKind::SNAPSHOT_EVENT_KIND_DELETED,
        };
        let e = agent::SnapshotEvent {
            workload: workload.to_owned(),
            volume: volume.to_owned(),
            snapshot: snapshot.to_owned(),
            name: name.to_owned(),
            event: event.into(),
            ..Default::default()
        };
        self.wiring
            .journal
            .append(Body::Snapshot(Box::new(e)))
            .await
            .map(|_| ())
            .map_err(|e| Failure(format!("snapshot {snapshot} could not be reported: {e}")))
    }

    async fn report_volume(&self, e: agent::VolumeEvent) -> Result<(), Failure> {
        let volume = e.helper_id.clone();
        self.wiring
            .journal
            .append(Body::Volume(Box::new(e)))
            .await
            .map(|_| ())
            .map_err(|e| Failure(format!("volume {volume} could not be reported: {e}")))
    }
}
