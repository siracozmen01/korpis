// SPDX-License-Identifier: AGPL-3.0-or-later

//! `PlanService`.

use super::wire::plan::plan_to_pb;
use super::wire::{self, Refuse, time};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::workload::{ApprovePlan, RejectPlan};
use korpis_core::id::IdKind;
use korpis_core::workload::Plan;
use korpis_proto::korpis::v1 as pb;

fn approved(p: &Plan) -> pb::ApprovePlanResponse {
    pb::ApprovePlanResponse { plan: plan_to_pb(p).into(), ..Default::default() }
}

fn rejected(p: &Plan) -> pb::RejectPlanResponse {
    pb::RejectPlanResponse { plan: plan_to_pb(p).into(), ..Default::default() }
}

fn cancelled(p: &Plan) -> pb::CancelPlanResponse {
    pb::CancelPlanResponse { plan: plan_to_pb(p).into(), ..Default::default() }
}

impl pb::PlanService for Api {
    async fn get_plan(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetPlanRequest>,
    ) -> ServiceResult<pb::GetPlanResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Plan, "id", request.id).refuse()?;
        let plan = self.app.get_plan(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetPlanResponse { plan: plan_to_pb(&plan).into(), ..Default::default() })
    }

    async fn list_plans(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListPlansRequest>,
    ) -> ServiceResult<pb::ListPlansResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = self.page(req.page.as_option(), IdKind::Plan)?;
        let workload = wire::id(IdKind::Workload, "workload", &req.workload).refuse()?;
        let listed = self.app.list_plans(&caller, workload, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListPlansResponse {
            plans: listed.items.iter().map(plan_to_pb).collect(),
            page: self.next_page(listed.next, IdKind::Plan)?.into(),
            ..Default::default()
        })
    }

    async fn approve_plan(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ApprovePlanRequest>,
    ) -> ServiceResult<pb::ApprovePlanResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let cmd = ApprovePlan {
            plan: wire::id(IdKind::Plan, "plan", &req.plan).refuse()?,
            apply_after: req
                .apply_after
                .as_option()
                .map(|t| time("apply_after", t))
                .transpose()
                .refuse()?,
        };
        let encoder = |p: &Plan| encode(&approved(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.approve_plan(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, approved)
    }

    async fn reject_plan(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RejectPlanRequest>,
    ) -> ServiceResult<pb::RejectPlanResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let cmd = RejectPlan {
            plan: wire::id(IdKind::Plan, "plan", &req.plan).refuse()?,
            reason: req.reason.clone(),
        };
        let encoder = |p: &Plan| encode(&rejected(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.reject_plan(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, rejected)
    }

    async fn cancel_plan(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CancelPlanRequest>,
    ) -> ServiceResult<pb::CancelPlanResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Plan, "plan", &req.plan).refuse()?;
        let encoder = |p: &Plan| encode(&cancelled(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.cancel_plan(&caller, &idem, id).await.map_err(error::from_app)?;
        respond(reply, cancelled)
    }
}
