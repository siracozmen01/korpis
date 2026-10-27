// SPDX-License-Identifier: AGPL-3.0-or-later

//! `EdgeGroupService` (§3.1 of docs/07-networking.md).

use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::edge_group::EdgeGroupSpec;
use korpis_app::idempotency::Idempotent;
use korpis_core::edge::EdgeGroup;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::edge::{group_to_pb, with_status};

fn created(g: &EdgeGroup) -> pb::CreateEdgeGroupResponse {
    pb::CreateEdgeGroupResponse { group: group_to_pb(g).into(), ..Default::default() }
}

fn updated(g: &EdgeGroup) -> pb::UpdateEdgeGroupResponse {
    pb::UpdateEdgeGroupResponse { group: group_to_pb(g).into(), ..Default::default() }
}

fn deleted() -> pb::DeleteEdgeGroupResponse {
    pb::DeleteEdgeGroupResponse::default()
}

fn port(field: &str, n: u32) -> Result<u16, ConnectError> {
    u16::try_from(n).map_err(|_| error::invalid(field, "a port"))
}

/// The group a request asks for, of `organization`; a setting left unset takes its default.
fn asked(g: &pb::EdgeGroup, organization: uuid::Uuid) -> Result<EdgeGroupSpec, ConnectError> {
    let members = g.members.iter().map(|m| wire::id(IdKind::Node, "group.members", m).refuse());
    let ports = match (g.first_port, g.last_port) {
        (0, 0) => None,
        (first, last) => Some((port("group.first_port", first)?, port("group.last_port", last)?)),
    };
    let duration = |field: &str, d: Option<&buffa_types::google::protobuf::Duration>| match d {
        Some(d) => korpis_wire::duration_from(field, d).refuse().map(Some),
        None => Ok(None),
    };
    Ok(EdgeGroupSpec {
        organization,
        name: g.name.clone(),
        members: members.collect::<Result<_, _>>()?,
        ports,
        probe_port: (g.probe_port != 0)
            .then(|| port("group.probe_port", g.probe_port))
            .transpose()?,
        probe_interval: duration("group.probe_interval", g.probe_interval.as_option())?,
        probe_misses: (g.probe_misses != 0).then_some(g.probe_misses),
        term: duration("group.term", g.term.as_option())?,
    })
}

impl pb::EdgeGroupService for Api {
    async fn create_edge_group(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateEdgeGroupRequest>,
    ) -> ServiceResult<pb::CreateEdgeGroupResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let group = req.group.as_option().ok_or_else(|| error::invalid("group", "required"))?;
        let organization =
            wire::id(IdKind::Organization, "group.organization", &group.organization).refuse()?;
        let spec = asked(group, organization)?;
        let encoder = |g: &EdgeGroup| encode(&created(g));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.create_edge_group(&caller, &idem, spec).await.map_err(error::from_app)?;
        respond(reply, created)
    }

    async fn get_edge_group(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetEdgeGroupRequest>,
    ) -> ServiceResult<pb::GetEdgeGroupResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let id = wire::id(IdKind::EdgeGroup, "id", &req.id).refuse()?;
        let group = self.app.edge_group(&caller, id).await.map_err(error::from_app)?;
        let (members, addresses) =
            self.app.edge_group_status(group.id).await.map_err(error::from_app)?;
        let mut shown = with_status(group_to_pb(&group), (&members, &addresses), self.app.now());
        let ingress = self.app.edge_group_ingress(group.id).await.map_err(error::from_app)?;
        shown.ingress_addresses = ingress.iter().map(ToString::to_string).collect();
        Response::ok(pb::GetEdgeGroupResponse { group: shown.into(), ..Default::default() })
    }

    async fn list_edge_groups(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListEdgeGroupsRequest>,
    ) -> ServiceResult<pb::ListEdgeGroupsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = self.page(req.page.as_option(), IdKind::EdgeGroup)?;
        let organization =
            wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let listed = self
            .app
            .list_edge_groups(&caller, organization, page)
            .await
            .map_err(error::from_app)?;
        Response::ok(pb::ListEdgeGroupsResponse {
            groups: listed.items.iter().map(group_to_pb).collect(),
            page: self.next_page(listed.next, IdKind::EdgeGroup)?.into(),
            ..Default::default()
        })
    }

    async fn update_edge_group(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::UpdateEdgeGroupRequest>,
    ) -> ServiceResult<pb::UpdateEdgeGroupResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let group = req.group.as_option().ok_or_else(|| error::invalid("group", "required"))?;
        let id = wire::id(IdKind::EdgeGroup, "group.id", &group.id).refuse()?;
        // Its organization and name stay, whatever is asked; the rest is asked for whole.
        let spec = asked(group, uuid::Uuid::nil())?;
        let encoder = |g: &EdgeGroup| encode(&updated(g));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .update_edge_group(&caller, &idem, (id, spec))
            .await
            .map_err(error::from_app)?;
        respond(reply, updated)
    }

    async fn delete_edge_group(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeleteEdgeGroupRequest>,
    ) -> ServiceResult<pb::DeleteEdgeGroupResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::EdgeGroup, "id", &req.id).refuse()?;
        let encoder = |(): &()| encode(&deleted());
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.delete_edge_group(&caller, &idem, id).await.map_err(error::from_app)?;
        respond(reply, |(): &()| deleted())
    }
}
