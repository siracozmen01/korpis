// SPDX-License-Identifier: AGPL-3.0-or-later

//! `OrganizationService.GetTrustPolicy` and `SetTrustPolicy` (§7 of docs/09-recipes.md).

use super::wire::recipe::{policy_from_pb, policy_to_pb};
use super::wire::{self, Refuse, principal, timestamp};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::tenancy::{Governing, SetTrustPolicy};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;

fn governing_to_pb(g: &Governing) -> pb::GoverningTrustPolicy {
    let set = g.set.as_ref();
    pb::GoverningTrustPolicy {
        policy: policy_to_pb(&g.policy()).into(),
        set_on: set.map(|s| wire::wire_id(IdKind::Organization, s.organization)),
        set_by: set.map(|s| principal::to_pb(&s.set_by)).into(),
        set_at: set.map(|s| timestamp(s.set_at)).into(),
        own_version: g.own_version,
        ..Default::default()
    }
}

fn set_response(g: &Governing) -> pb::SetTrustPolicyResponse {
    pb::SetTrustPolicyResponse { governing: governing_to_pb(g).into(), ..Default::default() }
}

/// The policy that governs the organization.
pub(super) async fn get(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::GetTrustPolicyRequest>,
) -> ServiceResult<pb::GetTrustPolicyResponse> {
    let caller = api.caller(ctx).await?;
    let id = wire::id(IdKind::Organization, "organization", request.organization).refuse()?;
    let g = api.app.get_trust_policy(&caller, id).await.map_err(error::from_app)?;
    Response::ok(pb::GetTrustPolicyResponse {
        governing: governing_to_pb(&g).into(),
        ..Default::default()
    })
}

/// Sets or removes the organization's own policy.
pub(super) async fn set(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::SetTrustPolicyRequest>,
) -> ServiceResult<pb::SetTrustPolicyResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let expected_version = req.mutation.as_option().and_then(|m| m.expected_version).unwrap_or(0);
    let cmd = SetTrustPolicy {
        organization: wire::id(IdKind::Organization, "organization", &req.organization).refuse()?,
        policy: req.policy.as_option().map(policy_from_pb),
        expected_version,
    };
    let encoder = |g: &Governing| encode(&set_response(g));
    let idem = Idempotent { key, encode: &encoder };
    let reply = api.app.set_trust_policy(&caller, &idem, cmd).await.map_err(error::from_app)?;
    respond(reply, set_response)
}
