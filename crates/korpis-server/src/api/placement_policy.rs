// SPDX-License-Identifier: AGPL-3.0-or-later

//! `OrganizationService.GetPlacementPolicy` and `SetPlacementPolicy` (§2.2 of
//! docs/05-scheduling.md).

use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::tenancy::PolicyAt;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::placement::{policy_from_pb, policy_to_pb};

fn at_to_pb(p: &PolicyAt) -> pb::OrganizationPlacementPolicy {
    pb::OrganizationPlacementPolicy {
        own: p.own.as_ref().map(policy_to_pb).into(),
        effective: Some(policy_to_pb(&p.effective)).into(),
        set_at: p.set_at.map(|o| wire::wire_id(IdKind::Organization, o)).unwrap_or_default(),
        ..Default::default()
    }
}

fn set_response(p: &PolicyAt) -> pb::SetPlacementPolicyResponse {
    pb::SetPlacementPolicyResponse { policy: at_to_pb(p).into(), ..Default::default() }
}

/// The policy at the organization.
pub(super) async fn get(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::GetPlacementPolicyRequest>,
) -> ServiceResult<pb::GetPlacementPolicyResponse> {
    let caller = api.caller(ctx).await?;
    let id = wire::id(IdKind::Organization, "organization", request.organization).refuse()?;
    let p = api.app.placement_policy(&caller, id).await.map_err(error::from_app)?;
    Response::ok(pb::GetPlacementPolicyResponse {
        policy: at_to_pb(&p).into(),
        ..Default::default()
    })
}

/// Sets, replaces, or removes the organization's own policy.
pub(super) async fn set(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::SetPlacementPolicyRequest>,
) -> ServiceResult<pb::SetPlacementPolicyResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let organization =
        wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
    let policy =
        req.policy.as_option().map(|p| policy_from_pb("policy", p)).transpose().refuse()?;
    let encoder = |p: &PolicyAt| encode(&set_response(p));
    let idem = Idempotent { key, encode: &encoder };
    let reply = api
        .app
        .set_placement_policy(&caller, &idem, (organization, policy))
        .await
        .map_err(error::from_app)?;
    respond(reply, set_response)
}
