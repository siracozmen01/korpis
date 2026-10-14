// SPDX-License-Identifier: AGPL-3.0-or-later

//! `VolumeService`: volumes as nodes reported them, through the project they belong to (§7 of
//! docs/06-storage.md). A size is shown only where the write path enforces it (Rule K-3).

use super::wire::{self, Refuse};
use super::{Api, error, next_page, page};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::volume::ListVolumes;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::volume::volume_to_pb;

impl pb::VolumeService for Api {
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
        let page = page(req.page.as_option(), IdKind::Volume)?;
        let project = wire::id(IdKind::Project, "project", &req.project).refuse()?;
        let workload = match req.workload.as_deref() {
            None | Some("") => None,
            Some(w) => Some(wire::id(IdKind::Workload, "workload", w).refuse()?),
        };
        let q = ListVolumes { project, workload, include_deleted: req.include_deleted };
        let listed = self.app.list_volumes(&caller, q, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListVolumesResponse {
            volumes: listed.items.iter().map(volume_to_pb).collect(),
            page: next_page(listed.next, IdKind::Volume).into(),
            ..Default::default()
        })
    }
}
