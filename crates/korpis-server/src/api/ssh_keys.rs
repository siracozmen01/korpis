// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ProfileService.ListSshKeys`, `AddSshKey`, and `RemoveSshKey` (§9.9 of docs/08-identity.md):
//! the SSH keys a signed-in person reaches their files with over SFTP.

use super::wire::{self, Refuse as _};
use super::{Api, error};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_core::account::ssh_key::SshKey;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::{timestamp, wire_id};

fn key_to_pb(k: &SshKey) -> pb::ProfileSshKey {
    pb::ProfileSshKey {
        id: wire_id(IdKind::SshKey, k.id),
        label: k.label.clone(),
        kind: k.key.kind().name().to_owned(),
        fingerprint: k.key.fingerprint(),
        added_at: Some(timestamp(k.added_at)).into(),
        last_used_at: k.last_used_at.map(timestamp).into(),
        within: k.within.map(|o| wire_id(IdKind::Organization, o)),
        ..Default::default()
    }
}

pub(super) async fn list(
    api: &Api,
    ctx: &RequestContext,
) -> ServiceResult<pb::ListSshKeysResponse> {
    let caller = api.caller(ctx).await?;
    let keys = api.app.ssh_keys(&caller).await.map_err(error::from_app)?;
    Response::ok(pb::ListSshKeysResponse {
        keys: keys.iter().map(key_to_pb).collect(),
        ..Default::default()
    })
}

pub(super) async fn add(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::AddSshKeyRequest>,
) -> ServiceResult<pb::AddSshKeyResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let key = api.app.add_ssh_key(&caller, &req.key, &req.label).await.map_err(error::from_app)?;
    Response::ok(pb::AddSshKeyResponse { key: Some(key_to_pb(&key)).into(), ..Default::default() })
}

pub(super) async fn remove(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::RemoveSshKeyRequest>,
) -> ServiceResult<pb::RemoveSshKeyResponse> {
    let caller = api.caller(ctx).await?;
    let id = wire::id(IdKind::SshKey, "id", request.id).refuse()?;
    api.app.remove_ssh_key(&caller, id).await.map_err(error::from_app)?;
    Response::ok(pb::RemoveSshKeyResponse::default())
}
