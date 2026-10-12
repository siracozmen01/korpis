// SPDX-License-Identifier: AGPL-3.0-or-later

//! `WorkloadService`.

use super::wire::Refuse;
use super::wire::plan::plan_to_pb;
use super::wire::spec::declare::declaration_from_pb;
use super::wire::tenancy::labels;
use super::wire::workload::{intent_to_pb, power_state_from_pb, power_to_pb, workload_to_pb};
use super::wire::{self, name};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{ConnectError, RequestContext, ServiceRequest, ServiceResult, ServiceStream};
use korpis_app::Idempotent;
use korpis_app::workload::{
    ChangePower, DeclareIntent, DeclareTarget, Declared, NewWorkload, PowerChanged,
};
use korpis_core::id::IdKind;
use korpis_core::workload::PowerChange;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;

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

    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn get_workload(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, pb::GetWorkloadRequest>,
    ) -> ServiceResult<pb::GetWorkloadResponse> {
        Err(unbuilt("GetWorkload"))
    }

    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn list_workloads(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, pb::ListWorkloadsRequest>,
    ) -> ServiceResult<pb::ListWorkloadsResponse> {
        Err(unbuilt("ListWorkloads"))
    }

    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn list_intents(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, pb::ListIntentsRequest>,
    ) -> ServiceResult<pb::ListIntentsResponse> {
        Err(unbuilt("ListIntents"))
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
