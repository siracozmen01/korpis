// SPDX-License-Identifier: AGPL-3.0-or-later

//! `RelayControlService.AuthorizeFiles` (§9.9 of docs/08-identity.md): a file token for an SFTP
//! client a relay serves, asked for under the relay's identity key over what it asks.

use super::agent::proof_from_pb;
use super::wire::{self, Refuse as _, timestamp};
use super::{Api, error};
use connectrpc::{Response, ServiceResult};
use korpis_app::relay::AuthorizeFiles;
use korpis_core::id::IdKind;
use korpis_proto::korpis::relay::v1 as relay;

/// A file token for the client the relay names, if the relay's proof is fresh and over this
/// request.
pub(super) async fn authorize(
    api: &Api,
    req: relay::AuthorizeFilesRequest,
) -> ServiceResult<relay::AuthorizeFilesResponse> {
    let id = wire::id(IdKind::Relay, "relay", &req.relay).refuse()?;
    let proof = proof_from_pb(&req.relay, req.auth.as_option())?;
    let client = req
        .client
        .parse()
        .map_err(|_| error::invalid("client", "the client's address, as the relay saw it"))?;
    let cmd = AuthorizeFiles {
        key: req.key,
        workload: wire::id(IdKind::Workload, "workload", &req.workload).refuse()?,
        client,
        renews: (!req.renews.is_empty()).then_some(req.renews),
    };
    let relay = api.app.sftp_relay(id, &proof, &cmd).await.map_err(error::from_app)?;
    let done = api.app.authorize_files(Some(relay), cmd).await.map_err(error::from_app)?;
    Response::ok(relay::AuthorizeFilesResponse {
        token: done.minted.token,
        expires_at: timestamp(done.minted.expires_at).into(),
        route: done.minted.route.unwrap_or_default(),
        action: done.action.to_owned(),
        ..Default::default()
    })
}
