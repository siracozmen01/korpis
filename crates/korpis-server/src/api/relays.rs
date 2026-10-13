// SPDX-License-Identifier: AGPL-3.0-or-later

//! Relays, as operators manage them through `NodeService` (§3.1 of docs/02-architecture.md):
//! enrollment tokens, the list, and removal. All of it is over the root organization.

use super::wire::relay::relay_to_pb;
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::relay::{CreateRelayEnrollment, RelayEnrollmentToken};
use korpis_core::id::IdKind;
use korpis_core::relay::{Relay, RelayUrl};
use korpis_proto::korpis::v1 as pb;

pub(super) async fn create_token(
    api: &Api,
    ctx: RequestContext,
    request: ServiceRequest<'_, pb::CreateRelayEnrollmentTokenRequest>,
) -> ServiceResult<pb::CreateRelayEnrollmentTokenResponse> {
    let caller = api.caller(&ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let cmd = CreateRelayEnrollment {
        organization: wire::id(IdKind::Organization, "organization", &req.organization).refuse()?,
        name: wire::name("name", &req.name).refuse()?,
        url: RelayUrl::parse(&req.url).map_err(|e| error::invalid("url", e.to_string()))?,
        expires_in: req
            .expires_in
            .as_option()
            .map(|d| wire::duration_from("expires_in", d))
            .transpose()
            .refuse()?,
    };
    // As for a node's token, the record kept for a replay leaves the secret out (§6 of
    // docs/10-api.md).
    let fingerprint = api.ca_fingerprint.clone();
    let response =
        |t: &RelayEnrollmentToken, secret: bool| pb::CreateRelayEnrollmentTokenResponse {
            token: if secret { t.secret.expose().to_owned() } else { String::new() },
            expires_at: wire::timestamp(t.enrollment.expires_at).into(),
            ca_fingerprint: fingerprint.clone(),
            ..Default::default()
        };
    let encoder = |t: &RelayEnrollmentToken| encode(&response(t, false));
    let idem = Idempotent { key, encode: &encoder };
    let reply =
        api.app.create_relay_enrollment(&caller, &idem, cmd).await.map_err(error::from_app)?;
    respond(reply, |t| response(t, true))
}

pub(super) async fn list(
    api: &Api,
    ctx: RequestContext,
    request: ServiceRequest<'_, pb::ListRelaysRequest>,
) -> ServiceResult<pb::ListRelaysResponse> {
    let caller = api.caller(&ctx).await?;
    let org = wire::id(IdKind::Organization, "organization", request.organization).refuse()?;
    let relays = api.app.list_relays(&caller, org).await.map_err(error::from_app)?;
    Response::ok(pb::ListRelaysResponse {
        relays: relays.iter().map(relay_to_pb).collect(),
        ..Default::default()
    })
}

pub(super) async fn remove(
    api: &Api,
    ctx: RequestContext,
    request: ServiceRequest<'_, pb::RemoveRelayRequest>,
) -> ServiceResult<pb::RemoveRelayResponse> {
    let caller = api.caller(&ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let org = wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
    let relay = wire::id(IdKind::Relay, "relay", &req.relay).refuse()?;
    let response =
        |r: &Relay| pb::RemoveRelayResponse { relay: relay_to_pb(r).into(), ..Default::default() };
    let encoder = |r: &Relay| encode(&response(r));
    let idem = Idempotent { key, encode: &encoder };
    let reply =
        api.app.remove_relay(&caller, &idem, (org, relay)).await.map_err(error::from_app)?;
    respond(reply, response)
}
