// SPDX-License-Identifier: AGPL-3.0-or-later

//! `VolumeService`: volumes as nodes reported them, through the project they belong to (§7 of
//! docs/06-storage.md). A size is shown only where the write path enforces it (Rule K-3).

use super::wire::plan::plan_to_pb;
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::idempotency::Idempotent;
use korpis_app::volume::ListVolumes;
use korpis_core::id::IdKind;
use korpis_core::value::Value;
use korpis_core::workload::Plan;
use korpis_core::workload::plan::VolumeOp;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::volume::{snapshot_to_pb, volume_to_pb};
use korpis_wire::wire_id;

fn proposed(p: &Plan) -> pb::DeleteVolumeResponse {
    pb::DeleteVolumeResponse { plan: plan_to_pb(p).into(), ..Default::default() }
}

/// The snapshot a plan taking one takes, by the identifier it gave it.
fn taken(p: &Plan) -> String {
    let id = p.steps.iter().find_map(|s| match s.detail.get("snapshot_id") {
        Some(Value::String(id)) => uuid::Uuid::parse_str(id).ok(),
        _ => None,
    });
    id.map(|id| wire_id(IdKind::Snapshot, id)).unwrap_or_default()
}

fn snapshotting(p: &Plan) -> pb::CreateSnapshotResponse {
    pb::CreateSnapshotResponse {
        plan: plan_to_pb(p).into(),
        snapshot: taken(p),
        ..Default::default()
    }
}

fn rolling_back(p: &Plan) -> pb::RollbackVolumeResponse {
    pb::RollbackVolumeResponse { plan: plan_to_pb(p).into(), ..Default::default() }
}

fn deleting_snapshot(p: &Plan) -> pb::DeleteSnapshotResponse {
    pb::DeleteSnapshotResponse { plan: plan_to_pb(p).into(), ..Default::default() }
}

impl pb::VolumeService for Api {
    async fn create_snapshot(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateSnapshotRequest>,
    ) -> ServiceResult<pb::CreateSnapshotResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Volume, "volume", &req.volume).refuse()?;
        let encoder = |p: &Plan| encode(&snapshotting(p));
        let idem = Idempotent { key, encode: &encoder };
        let asked = (id, req.name.as_str());
        let reply =
            self.app.create_snapshot(&caller, &idem, asked).await.map_err(error::from_app)?;
        respond(reply, snapshotting)
    }

    async fn list_snapshots(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListSnapshotsRequest>,
    ) -> ServiceResult<pb::ListSnapshotsResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Volume, "volume", request.volume).refuse()?;
        let listed = self.app.list_snapshots(&caller, id, request.include_deleted).await;
        let listed = listed.map_err(error::from_app)?;
        Response::ok(pb::ListSnapshotsResponse {
            snapshots: listed.iter().map(snapshot_to_pb).collect(),
            ..Default::default()
        })
    }

    async fn rollback_volume(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RollbackVolumeRequest>,
    ) -> ServiceResult<pb::RollbackVolumeResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Snapshot, "snapshot", &req.snapshot).refuse()?;
        let encoder = |p: &Plan| encode(&rolling_back(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.on_snapshot(&caller, &idem, (id, VolumeOp::Rollback)).await;
        respond(reply.map_err(error::from_app)?, rolling_back)
    }

    async fn delete_snapshot(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeleteSnapshotRequest>,
    ) -> ServiceResult<pb::DeleteSnapshotResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Snapshot, "id", &req.id).refuse()?;
        let encoder = |p: &Plan| encode(&deleting_snapshot(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.on_snapshot(&caller, &idem, (id, VolumeOp::DeleteSnapshot)).await;
        respond(reply.map_err(error::from_app)?, deleting_snapshot)
    }

    async fn delete_volume(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeleteVolumeRequest>,
    ) -> ServiceResult<pb::DeleteVolumeResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Volume, "id", &req.id).refuse()?;
        let encoder = |p: &Plan| encode(&proposed(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.delete_volume(&caller, &idem, id).await.map_err(error::from_app)?;
        respond(reply, proposed)
    }

    async fn get_volume(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetVolumeRequest>,
    ) -> ServiceResult<pb::GetVolumeResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Volume, "id", request.id).refuse()?;
        let v = self.app.get_volume(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetVolumeResponse {
            volume: volume_to_pb(&v).into(),
            ..Default::default()
        })
    }

    async fn list_volumes(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListVolumesRequest>,
    ) -> ServiceResult<pb::ListVolumesResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = self.page(req.page.as_option(), IdKind::Volume)?;
        let project = wire::id(IdKind::Project, "project", &req.project).refuse()?;
        let workload = match req.workload.as_deref() {
            None | Some("") => None,
            Some(w) => Some(wire::id(IdKind::Workload, "workload", w).refuse()?),
        };
        let q = ListVolumes { project, workload, include_deleted: req.include_deleted };
        let listed = self.app.list_volumes(&caller, q, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListVolumesResponse {
            volumes: listed.items.iter().map(volume_to_pb).collect(),
            page: self.next_page(listed.next, IdKind::Volume)?.into(),
            ..Default::default()
        })
    }
}
