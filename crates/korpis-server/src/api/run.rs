// SPDX-License-Identifier: AGPL-3.0-or-later

//! `RunService` (§4 of docs/05-scheduling.md): runs, read under `workload.read` on their
//! workload and cancelled under `workload.stop`.

use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::idempotency::Idempotent;
use korpis_core::id::IdKind;
use korpis_core::workload::run::Run;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::run::run_to_pb;

fn cancelled(r: &Run) -> pb::CancelRunResponse {
    pb::CancelRunResponse { run: run_to_pb(r).into(), ..Default::default() }
}

impl pb::RunService for Api {
    async fn get_run(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetRunRequest>,
    ) -> ServiceResult<pb::GetRunResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Run, "id", request.id).refuse()?;
        let run = self.app.get_run(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetRunResponse { run: run_to_pb(&run).into(), ..Default::default() })
    }

    async fn list_runs(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListRunsRequest>,
    ) -> ServiceResult<pb::ListRunsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Run)?;
        let workload = wire::id(IdKind::Workload, "workload", &req.workload).refuse()?;
        let listed = self.app.list_runs(&caller, workload, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListRunsResponse {
            runs: listed.items.iter().map(run_to_pb).collect(),
            page: next_page(listed.next, IdKind::Run).into(),
            ..Default::default()
        })
    }

    async fn cancel_run(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CancelRunRequest>,
    ) -> ServiceResult<pb::CancelRunResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Run, "run", &req.run).refuse()?;
        let encoder = |r: &Run| encode(&cancelled(r));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.cancel_run(&caller, &idem, id).await.map_err(error::from_app)?;
        respond(reply, cancelled)
    }
}
