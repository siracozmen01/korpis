// SPDX-License-Identifier: AGPL-3.0-or-later

//! `OperationService`.

use super::wire::operation::{kind_from_pb, operation_to_pb, policy_from_pb};
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::operation::{DecideOperation, Decision, OperationView, ProposeOperation};
use korpis_core::id::IdKind;
use korpis_core::operation::{Kind, Scope};
use korpis_proto::korpis::v1 as pb;
use uuid::Uuid;

fn shown((op, items): &OperationView) -> pb::Operation {
    operation_to_pb(op, items, true)
}

/// The scope a proposal names for an operation of `kind`.
fn scope_of(kind: Kind, req: &pb::ProposeOperationRequest) -> korpis_wire::Result<Scope> {
    match kind {
        Kind::Drain | Kind::Evacuate => Ok(Scope::Node(wire::id(IdKind::Node, "node", &req.node)?)),
        Kind::TemplateRollout => Ok(Scope::Template {
            template: wire::id(IdKind::Template, "template", &req.template)?,
            version: req.template_version,
        }),
    }
}

impl Api {
    /// Decides `decision` about the operation `operation`, answering as `respond_with` says.
    async fn decide<T: buffa::Message + Send + 'static>(
        &self,
        ctx: &RequestContext,
        (mutation, operation): (Option<&pb::Mutation>, &str),
        decision: Decision,
        respond_with: fn(pb::Operation) -> T,
    ) -> ServiceResult<T> {
        let caller = self.caller(ctx).await?;
        let key = mutation_key(mutation)?;
        let operation = wire::id(IdKind::Operation, "operation", operation).refuse()?;
        let response = |v: &OperationView| respond_with(shown(v));
        let encoder = |v: &OperationView| encode(&response(v));
        let idem = Idempotent { key, encode: &encoder };
        let cmd = DecideOperation { operation, decision };
        let reply =
            self.app.decide_operation(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, response)
    }
}

impl pb::OperationService for Api {
    async fn propose_operation(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ProposeOperationRequest>,
    ) -> ServiceResult<pb::ProposeOperationResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let kind = kind_from_pb("kind", req.kind).refuse()?;
        let cmd = ProposeOperation {
            kind,
            scope: scope_of(kind, &req).refuse()?,
            policy: policy_from_pb("policy", req.policy.as_option()).refuse()?,
        };
        let response = |v: &OperationView| pb::ProposeOperationResponse {
            operation: shown(v).into(),
            ..Default::default()
        };
        let encoder = |v: &OperationView| encode(&response(v));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.propose_operation(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, response)
    }

    async fn approve_operation(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ApproveOperationRequest>,
    ) -> ServiceResult<pb::ApproveOperationResponse> {
        let req = request.to_owned_message();
        let start_after = req
            .start_after
            .as_option()
            .map(|t| wire::time("start_after", t))
            .transpose()
            .refuse()?;
        let parent: Option<Uuid> = match req.parent.as_str() {
            "" => None,
            p => Some(wire::id(IdKind::Grant, "parent", p).refuse()?),
        };
        let decision = Decision::Approve { start_after, parent };
        let to = |operation: pb::Operation| pb::ApproveOperationResponse {
            operation: operation.into(),
            ..Default::default()
        };
        self.decide(&ctx, (req.mutation.as_option(), &req.operation), decision, to).await
    }

    async fn pause_operation(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::PauseOperationRequest>,
    ) -> ServiceResult<pb::PauseOperationResponse> {
        let req = request.to_owned_message();
        let to = |operation: pb::Operation| pb::PauseOperationResponse {
            operation: operation.into(),
            ..Default::default()
        };
        self.decide(&ctx, (req.mutation.as_option(), &req.operation), Decision::Pause, to).await
    }

    async fn resume_operation(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ResumeOperationRequest>,
    ) -> ServiceResult<pb::ResumeOperationResponse> {
        let req = request.to_owned_message();
        let to = |operation: pb::Operation| pb::ResumeOperationResponse {
            operation: operation.into(),
            ..Default::default()
        };
        self.decide(&ctx, (req.mutation.as_option(), &req.operation), Decision::Resume, to).await
    }

    async fn abort_operation(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::AbortOperationRequest>,
    ) -> ServiceResult<pb::AbortOperationResponse> {
        let req = request.to_owned_message();
        let decision = Decision::Abort(req.reason.clone());
        let to = |operation: pb::Operation| pb::AbortOperationResponse {
            operation: operation.into(),
            ..Default::default()
        };
        self.decide(&ctx, (req.mutation.as_option(), &req.operation), decision, to).await
    }

    async fn get_operation(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetOperationRequest>,
    ) -> ServiceResult<pb::GetOperationResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Operation, "id", request.id).refuse()?;
        let view = self.app.operation(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetOperationResponse {
            operation: shown(&view).into(),
            ..Default::default()
        })
    }

    async fn list_operations(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListOperationsRequest>,
    ) -> ServiceResult<pb::ListOperationsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = self.page(req.page.as_option(), IdKind::Operation)?;
        let node = match req.node.as_str() {
            "" => None,
            n => Some(wire::id(IdKind::Node, "node", n).refuse()?),
        };
        let listed = self.app.operations(&caller, node, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListOperationsResponse {
            operations: listed
                .items
                .iter()
                .map(|(op, items)| operation_to_pb(op, items, false))
                .collect(),
            page: self.next_page(listed.next, IdKind::Operation)?.into(),
            ..Default::default()
        })
    }
}
