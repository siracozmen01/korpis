// SPDX-License-Identifier: AGPL-3.0-or-later

//! `RelayControlService` (§3.1 of docs/02-architecture.md): what a relay running as its own
//! process asks of the control plane. Joining is authenticated by the enrollment token in the
//! request, as an agent's enrollment is.

use super::agent::key_to_pb;
use super::wire::{timestamp, wire_id};
use super::{Api, error};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::relay::JoinRelay;
use korpis_core::id::IdKind;
use korpis_proto::korpis::relay::v1 as relay;

impl relay::RelayControlService for Api {
    async fn join(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, relay::JoinRequest>,
    ) -> ServiceResult<relay::JoinResponse> {
        let req = request.to_owned_message();
        let cmd = JoinRelay {
            token: req.enrollment_token,
            identity_key: req.identity_key,
            tls_key: req.tls_key,
            source_ip: ctx.peer_addr().map(|a| a.ip()),
        };
        let joined = self.app.join_relay(cmd).await.map_err(error::from_app)?;
        Response::ok(relay::JoinResponse {
            relay: wire_id(IdKind::Relay, joined.relay.id),
            name: joined.relay.name.as_str().to_owned(),
            url: joined.relay.url.as_str().to_owned(),
            certificate_chain_pem: joined.certificate.chain_pem,
            certificate_expires_at: timestamp(joined.certificate.expires_at).into(),
            authority_pem: joined.authority_pem,
            token_keys: joined.token_keys.iter().map(key_to_pb).collect(),
            ..Default::default()
        })
    }
}
