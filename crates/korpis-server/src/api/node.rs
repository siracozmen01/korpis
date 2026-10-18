// SPDX-License-Identifier: AGPL-3.0-or-later

//! `NodeService`.

use super::wire::node::node_to_pb;
use super::wire::scope::selector_from_pb;
use super::wire::tenancy::labels;
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::node::{CreateEnrollment, EnrollmentToken, NodeView};
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

fn removed(v: &NodeView) -> pb::RemoveNodeResponse {
    pb::RemoveNodeResponse { node: view_to_pb(v).into(), ..Default::default() }
}

impl pb::NodeService for Api {
    async fn create_enrollment_token(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateEnrollmentTokenRequest>,
    ) -> ServiceResult<pb::CreateEnrollmentTokenResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let expires_in = req.expires_in.as_option();
        let cmd = CreateEnrollment {
            organization: wire::id(IdKind::Organization, "organization", &req.organization)
                .refuse()?,
            node_name: wire::name("node_name", &req.node_name).refuse()?,
            labels: labels("labels", &req.labels).refuse()?,
            expires_in: expires_in
                .map(|d| wire::duration_from("expires_in", d))
                .transpose()
                .refuse()?,
        };
        // The record kept for a replay leaves the secret out: a secret recorded is a secret
        // stored (§6 of docs/10-api.md).
        let fingerprint = self.ca_fingerprint.clone();
        let response = |t: &EnrollmentToken, secret: bool| pb::CreateEnrollmentTokenResponse {
            token: if secret { t.secret.expose().to_owned() } else { String::new() },
            expires_at: wire::timestamp(t.enrollment.expires_at).into(),
            ca_fingerprint: fingerprint.clone(),
            ..Default::default()
        };
        let encoder = |t: &EnrollmentToken| encode(&response(t, false));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.create_enrollment(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, |t| response(t, true))
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

    async fn remove_node(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RemoveNodeRequest>,
    ) -> ServiceResult<pb::RemoveNodeResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Node, "node", &req.node).refuse()?;
        let encoder = |v: &NodeView| encode(&removed(v));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .remove_node(&caller, &idem, id, req.compromised)
            .await
            .map_err(error::from_app)?;
        respond(reply, removed)
    }

    async fn create_relay_enrollment_token(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateRelayEnrollmentTokenRequest>,
    ) -> ServiceResult<pb::CreateRelayEnrollmentTokenResponse> {
        super::relays::create_token(self, ctx, request).await
    }

    async fn list_relays(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListRelaysRequest>,
    ) -> ServiceResult<pb::ListRelaysResponse> {
        super::relays::list(self, ctx, request).await
    }

    async fn remove_relay(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RemoveRelayRequest>,
    ) -> ServiceResult<pb::RemoveRelayResponse> {
        super::relays::remove(self, ctx, request).await
    }

    async fn readmit_relay(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ReadmitRelayRequest>,
    ) -> ServiceResult<pb::ReadmitRelayResponse> {
        super::relays::readmit(self, ctx, request).await
    }
}
