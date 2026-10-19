// SPDX-License-Identifier: AGPL-3.0-or-later

//! `OrganizationService.SetQuota` (§5.1 of docs/05-scheduling.md): an organization's quota set,
//! or one of its projects', set from above.

use super::wire::tenancy::{organization_to_pb, project_to_pb};
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::tenancy::{QuotaSetAt, SetQuota};
use korpis_core::id::IdKind;
use korpis_core::tenancy::quota::QuotaOwner;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::quota::{quota_set_from_pb, quota_set_to_pb};

fn set(at: &QuotaSetAt) -> pb::SetQuotaResponse {
    let quota = quota_set_to_pb(&at.quota);
    let mut organization = organization_to_pb(&at.organization);
    let project = at
        .project
        .as_ref()
        .map(|p| pb::Project { quota: quota.clone().into(), ..project_to_pb(p) });
    if project.is_none() {
        organization.quota = quota.into();
    }
    pb::SetQuotaResponse {
        organization: organization.into(),
        project: project.into(),
        ..Default::default()
    }
}

/// Replaces a quota set, from above.
pub(super) async fn set_quota(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::SetQuotaRequest>,
) -> ServiceResult<pb::SetQuotaResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let organization =
        wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
    let project = req
        .project
        .as_deref()
        .map(|p| wire::id(IdKind::Project, "project", p))
        .transpose()
        .refuse()?;
    let owner = project.map_or(QuotaOwner::Organization(organization), QuotaOwner::Project);
    let given = req.quota.as_option().cloned().unwrap_or_default();
    let quota = quota_set_from_pb("quota", owner, &given).refuse()?;
    let cmd = SetQuota { organization, project, quota };
    let encoder = |at: &QuotaSetAt| encode(&set(at));
    let idem = Idempotent { key, encode: &encoder };
    let reply = api.app.set_quota(&caller, &idem, cmd).await.map_err(error::from_app)?;
    respond(reply, set)
}
