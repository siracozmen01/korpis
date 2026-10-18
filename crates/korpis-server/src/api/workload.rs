// SPDX-License-Identifier: AGPL-3.0-or-later

//! `WorkloadService`.

use super::wire::Refuse;
use super::wire::plan::plan_to_pb;
use super::wire::scope::selector_from_pb;
use super::wire::spec::declare::declaration_from_pb;
use super::wire::tenancy::labels;
use super::wire::workload::{
    classification_from_pb, power_state_from_pb, power_to_pb, shown_intent_to_pb, workload_to_pb,
};
use super::wire::{self, name};
use super::{Api, ServerApp, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{
    ConnectError, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream,
};
use korpis_app::Idempotent;
use korpis_app::restore::ConfirmPlacement;
use korpis_app::workload::{
    ChangePower, Classified, DeclareIntent, DeclareTarget, Declared, DeleteWorkload, ListWorkloads,
    NewWorkload, PlaceWorkload, PowerChanged, SetClassification, Watch, WorkloadView,
};
use korpis_core::id::IdKind;
use korpis_core::placement::Placement;
use korpis_core::workload::{Plan, PowerChange};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use std::sync::Arc;
use view::{classified, confirmed, placed, set_name, status, through_set};

mod view;

/// The watch as a stream: the status now, then each status that differs, until the client
/// leaves, the feed ends, or a read is refused, which ends the stream with that refusal.
fn watched(
    app: Arc<ServerApp>,
    caller: korpis_app::Caller,
    first: &WorkloadView,
    watch: Watch<korpis_store::Subscription>,
) -> impl futures_util::Stream<Item = Result<pb::WatchWorkloadResponse, ConnectError>> + Send + use<>
{
    let now = pb::WatchWorkloadResponse { status: status(first).into(), ..Default::default() };
    let rest = futures_util::stream::unfold(Some((app, caller, watch)), |state| async move {
        let (app, caller, mut watch) = state?;
        match app.next_status(&caller, &mut watch).await {
            Ok(Some(v)) => {
                let item =
                    pb::WatchWorkloadResponse { status: status(&v).into(), ..Default::default() };
                Some((Ok(item), Some((app, caller, watch))))
            }
            Ok(None) => None,
            Err(e) => Some((Err(error::from_app(e)), None)),
        }
    });
    futures_util::StreamExt::chain(futures_util::stream::once(async move { Ok(now) }), rest)
}

fn deleted(p: &Plan) -> pb::DeleteWorkloadResponse {
    pb::DeleteWorkloadResponse { plan: plan_to_pb(p).into(), ..Default::default() }
}

fn declared(d: &Declared) -> pb::DeclareIntentResponse {
    pb::DeclareIntentResponse {
        workload: workload_to_pb(&d.workload, &d.power).into(),
        intent: shown_intent_to_pb(&d.intent, &d.withheld).into(),
        plan: plan_to_pb(&d.plan).into(),
        ..Default::default()
    }
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
        (workload, propose): (&str, bool),
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
            propose,
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
            propose: req.propose,
            set: through_set(&req)?,
            keep: req.keep.clone(),
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
        let target = (req.workload.as_str(), req.propose);
        self.power(&ctx, req.mutation.as_option(), target, PowerChange::Start).await
    }

    async fn stop(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::StopRequest>,
    ) -> ServiceResult<pb::PowerResponse> {
        let req = request.to_owned_message();
        let target = (req.workload.as_str(), req.propose);
        self.power(&ctx, req.mutation.as_option(), target, PowerChange::Stop).await
    }

    async fn restart(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RestartRequest>,
    ) -> ServiceResult<pb::PowerResponse> {
        let req = request.to_owned_message();
        let target = (req.workload.as_str(), req.propose);
        self.power(&ctx, req.mutation.as_option(), target, PowerChange::Restart).await
    }

    async fn place_workload(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::PlaceWorkloadRequest>,
    ) -> ServiceResult<pb::PlaceWorkloadResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let cmd = PlaceWorkload {
            workload: wire::id(IdKind::Workload, "workload", &req.workload).refuse()?,
            node: wire::id(IdKind::Node, "node", &req.node).refuse()?,
            reason: req.reason.clone(),
        };
        let encoder = |p: &Placement| encode(&placed(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.place_workload(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, placed)
    }

    async fn confirm_placement(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ConfirmPlacementRequest>,
    ) -> ServiceResult<pb::ConfirmPlacementResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let cmd = ConfirmPlacement {
            workload: wire::id(IdKind::Workload, "workload", &req.workload).refuse()?,
            reason: req.reason.clone(),
        };
        let encoder = |p: &Placement| encode(&confirmed(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.confirm_placement(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, confirmed)
    }

    async fn set_classification(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetClassificationRequest>,
    ) -> ServiceResult<pb::SetClassificationResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let mutation = req.mutation.as_option();
        let key = mutation_key(mutation)?;
        let cmd = SetClassification {
            workload: wire::id(IdKind::Workload, "workload", &req.workload).refuse()?,
            expected_version: mutation.and_then(|m| m.expected_version).ok_or_else(|| {
                error::invalid("mutation.expected_version", "the workload's version")
            })?,
            overrides: classification_from_pb("overrides", &req.overrides).refuse()?,
            reason: req.reason.clone(),
        };
        let encoder = |c: &Classified| encode(&classified(c));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.set_classification(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, classified)
    }

    async fn delete_workload(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeleteWorkloadRequest>,
    ) -> ServiceResult<pb::DeleteWorkloadResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let mutation = req.mutation.as_option();
        let key = mutation_key(mutation)?;
        let cmd = DeleteWorkload {
            workload: wire::id(IdKind::Workload, "workload", &req.workload).refuse()?,
            expected_version: mutation.and_then(|m| m.expected_version),
            delete_volumes: req.delete_volumes.clone(),
        };
        let encoder = |p: &Plan| encode(&deleted(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.delete_workload(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, deleted)
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

    async fn get_workload_recipe(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetWorkloadRecipeRequest>,
    ) -> ServiceResult<pb::GetWorkloadRecipeResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Workload, "workload", request.workload).refuse()?;
        let locales: Vec<String> = request.locales.iter().map(|l| (*l).to_owned()).collect();
        let r = self.app.workload_recipe(&caller, id, &locales).await.map_err(error::from_app)?;
        Response::ok(pb::GetWorkloadRecipeResponse {
            recipe: korpis_wire::recipe::ref_to_pb(&r.reference).into(),
            metadata: korpis_wire::recipe_schema::metadata_to_pb(&r.recipe.metadata).into(),
            config: korpis_wire::recipe_schema::schema_to_pb(&r.recipe.config).into(),
            locale: r.locale,
            messages: r.messages.into_iter().collect(),
            ..Default::default()
        })
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
            set: set_name("set", req.set.as_deref().unwrap_or_default())?,
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
            intents: listed
                .items
                .iter()
                .map(|s| shown_intent_to_pb(&s.intent, &s.withheld))
                .collect(),
            page: next_page(listed.next, IdKind::Intent).into(),
            ..Default::default()
        })
    }

    async fn watch_workload(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::WatchWorkloadRequest>,
    ) -> ServiceResult<ServiceStream<pb::WatchWorkloadResponse>> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Workload, "workload", request.workload).refuse()?;
        let (first, watch) = self.app.watch_workload(&caller, id).await.map_err(error::from_app)?;
        Response::stream_ok(watched(Arc::clone(&self.app), caller, &first, watch))
    }
}
