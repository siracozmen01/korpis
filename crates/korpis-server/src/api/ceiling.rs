// SPDX-License-Identifier: AGPL-3.0-or-later

//! `OrganizationService.GetDelegationCeiling` and `SetDelegationCeiling` (§5.1 of
//! docs/08-identity.md).

use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::tenancy::Ceiling;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;

fn ceiling_to_pb(c: &Ceiling) -> pb::DelegationCeiling {
    pb::DelegationCeiling {
        own: c.own.map(wire::duration).into(),
        effective: Some(wire::duration(c.effective)).into(),
        ..Default::default()
    }
}

fn set_response(c: &Ceiling) -> pb::SetDelegationCeilingResponse {
    pb::SetDelegationCeilingResponse { ceiling: ceiling_to_pb(c).into(), ..Default::default() }
}

/// The ceiling at the organization.
pub(super) async fn get(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::GetDelegationCeilingRequest>,
) -> ServiceResult<pb::GetDelegationCeilingResponse> {
    let caller = api.caller(ctx).await?;
    let id = wire::id(IdKind::Organization, "organization", request.organization).refuse()?;
    let c = api.app.delegation_ceiling(&caller, id).await.map_err(error::from_app)?;
    Response::ok(pb::GetDelegationCeilingResponse {
        ceiling: ceiling_to_pb(&c).into(),
        ..Default::default()
    })
}

/// Sets or removes the organization's own ceiling.
pub(super) async fn set(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::SetDelegationCeilingRequest>,
) -> ServiceResult<pb::SetDelegationCeilingResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let organization =
        wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
    let ceiling =
        req.ceiling.as_option().map(|d| wire::duration_from("ceiling", d)).transpose().refuse()?;
    let encoder = |c: &Ceiling| encode(&set_response(c));
    let idem = Idempotent { key, encode: &encoder };
    let reply = api
        .app
        .set_delegation_ceiling(&caller, &idem, (organization, ceiling))
        .await
        .map_err(error::from_app)?;
    respond(reply, set_response)
}
