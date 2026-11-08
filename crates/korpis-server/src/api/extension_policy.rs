// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ExtensionService.GetExtensionPolicy` and `SetExtensionPolicy`: a root organization's rule over
//! the extensions installed beneath it.

use super::wire::recipe::{policy_from_pb, policy_to_pb};
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::extension::{GoverningExtensions, SetPolicy};
use korpis_core::extension::policy::ExtensionPolicy;
use korpis_core::id::IdKind;
use korpis_core::recipe::trust::TrustPolicy;
use korpis_proto::korpis::v1 as pb;

fn policy_pb(p: &ExtensionPolicy) -> pb::ExtensionPolicy {
    let signers = TrustPolicy { accepted_signers: p.publishers.clone(), ..TrustPolicy::default() };
    pb::ExtensionPolicy {
        registries: p.registries.clone(),
        publishers: policy_to_pb(&signers).accepted_signers,
        tenants_install: p.tenants_install,
        ..Default::default()
    }
}

/// A policy as sent; a publisher's fingerprint is output only, computed by the use case.
fn policy_from(p: &pb::ExtensionPolicy) -> ExtensionPolicy {
    let signers = pb::TrustPolicy { accepted_signers: p.publishers.clone(), ..Default::default() };
    ExtensionPolicy {
        registries: p.registries.clone(),
        publishers: policy_from_pb(&signers).accepted_signers,
        tenants_install: p.tenants_install,
    }
}

fn set_response(g: &GoverningExtensions) -> pb::SetExtensionPolicyResponse {
    pb::SetExtensionPolicyResponse {
        policy: policy_pb(&g.policy).into(),
        version: g.version,
        ..Default::default()
    }
}

/// The policy a root organization governs by.
pub(super) async fn get(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::GetExtensionPolicyRequest>,
) -> ServiceResult<pb::GetExtensionPolicyResponse> {
    let caller = api.caller(ctx).await?;
    let id = wire::id(IdKind::Organization, "organization", request.organization).refuse()?;
    let g = api.app.get_extension_policy(&caller, id).await.map_err(error::from_app)?;
    Response::ok(pb::GetExtensionPolicyResponse {
        policy: policy_pb(&g.policy).into(),
        version: g.version,
        ..Default::default()
    })
}

/// Sets a root organization's policy.
pub(super) async fn set(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::SetExtensionPolicyRequest>,
) -> ServiceResult<pb::SetExtensionPolicyResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let expected_version = req.mutation.as_option().and_then(|m| m.expected_version).unwrap_or(0);
    let cmd = SetPolicy {
        organization: wire::id(IdKind::Organization, "organization", &req.organization).refuse()?,
        policy: req.policy.as_option().map(policy_from).unwrap_or_default(),
        expected_version,
    };
    let encoder = |g: &GoverningExtensions| encode(&set_response(g));
    let idem = Idempotent { key, encode: &encoder };
    let reply = api.app.set_extension_policy(&caller, &idem, cmd).await.map_err(error::from_app)?;
    respond(reply, set_response)
}
