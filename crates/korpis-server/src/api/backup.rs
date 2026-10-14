// SPDX-License-Identifier: AGPL-3.0-or-later

//! `BackupService` (§5 of docs/06-storage.md): backups, authorized on the workload whose volume
//! they are of.

use super::wire::plan::plan_to_pb;
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::idempotency::Idempotent;
use korpis_core::id::IdKind;
use korpis_core::value::Value;
use korpis_core::workload::Plan;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::repository::backup_to_pb;
use korpis_wire::wire_id;

/// The plan, and the backup it takes as its step names it; `GetBackup` reads the rest.
fn proposed(p: &Plan) -> pb::CreateBackupResponse {
    let id = p.steps.iter().find_map(|s| match s.detail.get("backup") {
        Some(Value::String(id)) => uuid::Uuid::parse_str(id).ok(),
        _ => None,
    });
    let backup = pb::Backup {
        id: id.map(|id| wire_id(IdKind::Backup, id)).unwrap_or_default(),
        ..Default::default()
    };
    pb::CreateBackupResponse {
        plan: plan_to_pb(p).into(),
        backup: backup.into(),
        ..Default::default()
    }
}

impl pb::BackupService for Api {
    async fn create_backup(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateBackupRequest>,
    ) -> ServiceResult<pb::CreateBackupResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let volume = wire::id(IdKind::Volume, "volume", &req.volume).refuse()?;
        let repository = wire::id(IdKind::Repository, "repository", &req.repository).refuse()?;
        let encoder = |p: &Plan| encode(&proposed(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .create_backup(&caller, &idem, (volume, repository))
            .await
            .map_err(error::from_app)?;
        respond(reply, proposed)
    }

    async fn get_backup(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetBackupRequest>,
    ) -> ServiceResult<pb::GetBackupResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Backup, "id", request.id).refuse()?;
        let b = self.app.get_backup(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetBackupResponse {
            backup: backup_to_pb(&b).into(),
            ..Default::default()
        })
    }

    async fn list_backups(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListBackupsRequest>,
    ) -> ServiceResult<pb::ListBackupsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Backup)?;
        let workload = wire::id(IdKind::Workload, "workload", &req.workload).refuse()?;
        let listed = self
            .app
            .list_backups(&caller, (workload, req.include_deleted), page)
            .await
            .map_err(error::from_app)?;
        Response::ok(pb::ListBackupsResponse {
            backups: listed.items.iter().map(backup_to_pb).collect(),
            page: next_page(listed.next, IdKind::Backup).into(),
            ..Default::default()
        })
    }
}
