// SPDX-License-Identifier: AGPL-3.0-or-later

//! `NodeService`.

use super::wire::node::node_to_pb;
use super::wire::scope::selector_from_pb;
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::node::NodeView;
use korpis_app::{AppError, Idempotent};
use korpis_core::id::IdKind;
use korpis_core::node::NodeAdmin;
use korpis_proto::korpis::v1 as pb;

fn view_to_pb(v: &NodeView) -> pb::Node {
    node_to_pb(&v.node, &v.allocated)
}

fn cordoned(v: &NodeView) -> pb::CordonNodeResponse {
    pb::CordonNodeResponse { node: view_to_pb(v).into(), ..Default::default() }
}

fn uncordoned(v: &NodeView) -> pb::UncordonNodeResponse {
    pb::UncordonNodeResponse { node: view_to_pb(v).into(), ..Default::default() }
}

impl pb::NodeService for Api {
    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn create_enrollment_token(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::CreateEnrollmentTokenRequest>,
    ) -> ServiceResult<pb::CreateEnrollmentTokenResponse> {
        // Enrollment lands with the agent protocol's server side, later in Phase 0.
        Err(error::from_app(AppError::NotYet { what: "node enrollment tokens", phase: 0 }))
    }

    async fn get_node(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetNodeRequest>,
    ) -> ServiceResult<pb::GetNodeResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Node, "id", request.id).refuse()?;
        let view = self.app.get_node(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetNodeResponse { node: view_to_pb(&view).into(), ..Default::default() })
    }

    async fn list_nodes(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListNodesRequest>,
    ) -> ServiceResult<pb::ListNodesResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Node)?;
        let org = wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let selector = selector_from_pb("selector", req.selector.as_option()).refuse()?;
        let listed =
            self.app.list_nodes(&caller, org, &selector, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListNodesResponse {
            nodes: listed.items.iter().map(view_to_pb).collect(),
            page: next_page(listed.next, IdKind::Node).into(),
            ..Default::default()
        })
    }

    async fn cordon_node(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CordonNodeRequest>,
    ) -> ServiceResult<pb::CordonNodeResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Node, "node", &req.node).refuse()?;
        let encoder = |v: &NodeView| encode(&cordoned(v));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .set_node_admin(&caller, &idem, id, NodeAdmin::Cordoned)
            .await
            .map_err(error::from_app)?;
        respond(reply, cordoned)
    }

    async fn uncordon_node(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::UncordonNodeRequest>,
    ) -> ServiceResult<pb::UncordonNodeResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Node, "node", &req.node).refuse()?;
        let encoder = |v: &NodeView| encode(&uncordoned(v));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .set_node_admin(&caller, &idem, id, NodeAdmin::Active)
            .await
            .map_err(error::from_app)?;
        respond(reply, uncordoned)
    }
}
