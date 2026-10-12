// SPDX-License-Identifier: AGPL-3.0-or-later

//! `WorkloadService`.

use super::wire::Refuse;
use super::wire::placement::{bound_by_to_pb, explanation_to_pb};
use super::wire::plan::plan_to_pb;
use super::wire::scope::selector_from_pb;
use super::wire::spec::declare::declaration_from_pb;
use super::wire::spec::tier_to_pb;
use super::wire::tenancy::labels;
use super::wire::workload::{
    convergence_to_pb, intent_to_pb, power_state_from_pb, power_to_pb, workload_to_pb,
};
use super::wire::{self, name, timestamp, wire_id};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{
    ConnectError, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream,
};
use korpis_app::Idempotent;
use korpis_app::workload::{
    ChangePower, DeclareIntent, DeclareTarget, Declared, ListWorkloads, NewWorkload, PlacementView,
    PowerChanged, WorkloadView,
};
use korpis_core::id::IdKind;
use korpis_core::workload::PowerChange;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;

/// Everything a client needs to render one workload (§4.3 of docs/02-architecture.md), its
/// placement as the caller may see it. The observation is empty until a node reports one.
fn status(v: &WorkloadView) -> pb::WorkloadStatus {
    pb::WorkloadStatus {
        workload: workload_to_pb(&v.workload, &v.power).into(),
        target: v.target.as_ref().map(intent_to_pb).into(),
        head: intent_to_pb(&v.head).into(),
        convergence: convergence_to_pb(v.convergence).into(),
        active_plan: v.active_plan.as_ref().map(plan_to_pb).into(),
        placement: v.placement.as_ref().map(|p| placement_to_pb(v.workload.id, p)).into(),
        unplaced: v
            .unplaced
            .as_ref()
            .map(|u| pb::Unplaced {
                intent: wire_id(IdKind::Intent, u.intent),
                attempted_at: timestamp(u.attempted_at).into(),
                explanation: explanation_to_pb(&u.explanation).into(),
                ..Default::default()
            })
            .into(),
        ..Default::default()
    }
}

fn placement_to_pb(workload: uuid::Uuid, p: &PlacementView) -> pb::Placement {
    pb::Placement {
        workload: wire_id(IdKind::Workload, workload),
        node: p.node.map(|n| wire_id(IdKind::Node, n)).unwrap_or_default(),
        bound_at: timestamp(p.bound_at).into(),
        bound_by: bound_by_to_pb(p.bound_by).into(),
        reason: p.reason.clone(),
        explanation: explanation_to_pb(&p.explanation).into(),
        authoritative: p.authoritative,
        region: p.region.clone().unwrap_or_default(),
        tier: tier_to_pb(p.tier).into(),
        intent: wire_id(IdKind::Intent, p.intent),
        ..Default::default()
    }
}

fn declared(d: &Declared) -> pb::DeclareIntentResponse {
    pb::DeclareIntentResponse {
        workload: workload_to_pb(&d.workload, &d.power).into(),
        intent: intent_to_pb(&d.intent).into(),
        plan: plan_to_pb(&d.plan).into(),
        ..Default::default()
    }
}

/// A method of the service that is specified and not yet built.
fn unbuilt(method: &str) -> ConnectError {
    ConnectError::unimplemented(format!("{method} is not built yet"))
}

fn target(req: &pb::DeclareIntentRequest) -> Result<DeclareTarget, ConnectError> {
    let expected = req.mutation.as_option().and_then(|m| m.expected_version);
    match req.target.as_ref().ok_or_else(|| error::invalid("target", "required"))? {
        Target::Workload(w) => Ok(DeclareTarget::Existing {
            workload: wire::id(IdKind::Workload, "workload", w).refuse()?,
            expected_version: expected.ok_or_else(|| {
                error::invalid("mutation.expected_version", "required to change a workload")
            })?,
        }),
        Target::Create(n) => {
            if expected.is_some() {
                return Err(error::invalid("mutation.expected_version", "absent on create"));
            }
            Ok(DeclareTarget::New(NewWorkload {
                project: wire::id(IdKind::Project, "create.project", &n.project).refuse()?,
                name: name("create.name", &n.name).refuse()?,
                labels: labels("create.labels", &n.labels).refuse()?,
                power: power_state_from_pb("create.power", n.power).refuse()?,
            }))
        }
    }
}

fn power_response(c: &PowerChanged) -> pb::PowerResponse {
    pb::PowerResponse {
        power: power_to_pb(&c.power).into(),
        plan: plan_to_pb(&c.plan).into(),
        ..Default::default()
    }
}

impl Api {
    async fn power(
        &self,
        ctx: &RequestContext,
        mutation: Option<&pb::Mutation>,
        workload: &str,
        change: PowerChange,
    ) -> ServiceResult<pb::PowerResponse> {
        let caller = self.caller(ctx).await?;
        let key = mutation_key(mutation)?;
        let cmd = ChangePower {
            workload: wire::id(IdKind::Workload, "workload", workload).refuse()?,
            change,
            expected_version: mutation.and_then(|m| m.expected_version).ok_or_else(|| {
                error::invalid("mutation.expected_version", "the power version changed against")
            })?,
        };
        let encoder = |c: &PowerChanged| encode(&power_response(c));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.change_power(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, power_response)
    }
}

impl pb::WorkloadService for Api {
    async fn declare_intent(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeclareIntentRequest>,
    ) -> ServiceResult<pb::DeclareIntentResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let body = req.body.as_option().ok_or_else(|| error::invalid("body", "required"))?;
        let (recipe, declaration) = declaration_from_pb("body", body).refuse()?;
        let cmd = DeclareIntent {
            target: target(&req)?,
            recipe_name: recipe.name,
            recipe_version: recipe.version,
            declaration,
            dry_run: req.dry_run,
        };
        let encoder = |d: &Declared| encode(&declared(d));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.declare_intent(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, declared)
    }

    async fn start(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::StartRequest>,
    ) -> ServiceResult<pb::PowerResponse> {
        let req = request.to_owned_message();
        self.power(&ctx, req.mutation.as_option(), &req.workload, PowerChange::Start).await
    }

    async fn stop(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::StopRequest>,
    ) -> ServiceResult<pb::PowerResponse> {
        let req = request.to_owned_message();
        self.power(&ctx, req.mutation.as_option(), &req.workload, PowerChange::Stop).await
    }

    async fn restart(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RestartRequest>,
    ) -> ServiceResult<pb::PowerResponse> {
        let req = request.to_owned_message();
        self.power(&ctx, req.mutation.as_option(), &req.workload, PowerChange::Restart).await
    }

    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn delete_workload(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, pb::DeleteWorkloadRequest>,
    ) -> ServiceResult<pb::DeleteWorkloadResponse> {
        Err(unbuilt("DeleteWorkload"))
    }

    async fn get_workload(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetWorkloadRequest>,
    ) -> ServiceResult<pb::GetWorkloadResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Workload, "id", request.id).refuse()?;
        let view = self.app.get_workload(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetWorkloadResponse { status: status(&view).into(), ..Default::default() })
    }

    async fn list_workloads(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListWorkloadsRequest>,
    ) -> ServiceResult<pb::ListWorkloadsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Workload)?;
        let q = ListWorkloads {
            project: req
                .project
                .as_deref()
                .map(|p| wire::id(IdKind::Project, "project", p))
                .transpose()
                .refuse()?,
            organization: req
                .organization
                .as_deref()
                .map(|o| wire::id(IdKind::Organization, "organization", o))
                .transpose()
                .refuse()?,
            selector: selector_from_pb("selector", req.selector.as_option()).refuse()?,
        };
        let listed = self.app.list_workloads(&caller, &q, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListWorkloadsResponse {
            workloads: listed.items.iter().map(status).collect(),
            page: next_page(listed.next, IdKind::Workload).into(),
            ..Default::default()
        })
    }

    async fn list_intents(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListIntentsRequest>,
    ) -> ServiceResult<pb::ListIntentsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Intent)?;
        let workload = wire::id(IdKind::Workload, "workload", &req.workload).refuse()?;
        let listed =
            self.app.list_intents(&caller, workload, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListIntentsResponse {
            intents: listed.items.iter().map(intent_to_pb).collect(),
            page: next_page(listed.next, IdKind::Intent).into(),
            ..Default::default()
        })
    }

    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn watch_workload(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, pb::WatchWorkloadRequest>,
    ) -> ServiceResult<ServiceStream<pb::WatchWorkloadResponse>> {
        Err(unbuilt("WatchWorkload"))
    }
}
