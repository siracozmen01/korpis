// SPDX-License-Identifier: AGPL-3.0-or-later

//! `OrganizationService.SuspendOrganization` and `ResumeOrganization` (§2.1 of
//! docs/08-identity.md).

use super::wire::tenancy::{on_suspend, organization_to_pb};
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::tenancy::SuspendOrganization;
use korpis_core::id::IdKind;
use korpis_core::tenancy::Organization;
use korpis_proto::korpis::v1 as pb;

fn suspended(o: &Organization) -> pb::SuspendOrganizationResponse {
    pb::SuspendOrganizationResponse {
        organization: organization_to_pb(o).into(),
        ..Default::default()
    }
}

fn resumed(o: &Organization) -> pb::ResumeOrganizationResponse {
    pb::ResumeOrganizationResponse {
        organization: organization_to_pb(o).into(),
        ..Default::default()
    }
}

/// Suspends an organization from above.
pub(super) async fn suspend(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::SuspendOrganizationRequest>,
) -> ServiceResult<pb::SuspendOrganizationResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let cmd = SuspendOrganization {
        organization: wire::id(IdKind::Organization, "organization", &req.organization).refuse()?,
        reason: req.reason.clone(),
        on_suspend: on_suspend(req.on_suspend.as_known().unwrap_or_default()),
    };
    let encoder = |o: &Organization| encode(&suspended(o));
    let idem = Idempotent { key, encode: &encoder };
    let reply = api.app.suspend_organization(&caller, &idem, cmd).await.map_err(error::from_app)?;
    respond(reply, suspended)
}

/// Resumes a suspended organization from above.
pub(super) async fn resume(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::ResumeOrganizationRequest>,
) -> ServiceResult<pb::ResumeOrganizationResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let id = wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
    let encoder = |o: &Organization| encode(&resumed(o));
    let idem = Idempotent { key, encode: &encoder };
    let reply = api.app.resume_organization(&caller, &idem, id).await.map_err(error::from_app)?;
    respond(reply, resumed)
}
