// SPDX-License-Identifier: AGPL-3.0-or-later

//! `BackupService` (§5 of docs/06-storage.md): backups, authorized on the workload whose volume
//! they are of.

use super::wire::plan::plan_to_pb;
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream};
use korpis_app::backup::restore::RestoreAsk;
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

    async fn restore_backup(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RestoreBackupRequest>,
    ) -> ServiceResult<pb::RestoreBackupResponse> {
        use pb::__buffa::oneof::restore_backup_request::Target;
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let backup = wire::id(IdKind::Backup, "backup", &req.backup).refuse()?;
        let volume = match req.volume.as_str() {
            "" => None,
            v => Some(wire::id(IdKind::Volume, "volume", v).refuse()?),
        };
        let into = match &req.target {
            Some(Target::Into(into)) => Some(into.as_str()),
            Some(Target::Replace(true)) => None,
            _ => {
                let why = "into a new directory, or replacing the volume";
                return Err(error::invalid("target", why));
            }
        };
        let ask = RestoreAsk { backup, volume, path: &req.path, into };
        let restored = |p: &Plan| pb::RestoreBackupResponse {
            plan: plan_to_pb(p).into(),
            ..Default::default()
        };
        let encoder = |p: &Plan| encode(&restored(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.restore_backup(&caller, &idem, ask).await.map_err(error::from_app)?;
        respond(reply, restored)
    }

    async fn prune_backups(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::PruneBackupsRequest>,
    ) -> ServiceResult<pb::PruneBackupsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let workload = wire::id(IdKind::Workload, "workload", &req.workload).refuse()?;
        let repository = wire::id(IdKind::Repository, "repository", &req.repository).refuse()?;
        let pruned = |p: &Plan| pb::PruneBackupsResponse {
            plan: plan_to_pb(p).into(),
            ..Default::default()
        };
        let encoder = |p: &Plan| encode(&pruned(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .propose_prune(&caller, &idem, (workload, repository))
            .await
            .map_err(error::from_app)?;
        respond(reply, pruned)
    }

    async fn browse_backup(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::BrowseBackupRequest>,
    ) -> ServiceResult<pb::BrowseBackupResponse> {
        self.browse(ctx, request).await
    }

    async fn read_backup_file(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ReadBackupFileRequest>,
    ) -> ServiceResult<ServiceStream<pb::ReadBackupFileResponse>> {
        self.read_file(ctx, request).await
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
        let page = self.page(req.page.as_option(), IdKind::Backup)?;
        let workload = wire::id(IdKind::Workload, "workload", &req.workload).refuse()?;
        let listed = self
            .app
            .list_backups(&caller, (workload, req.include_deleted), page)
            .await
            .map_err(error::from_app)?;
        Response::ok(pb::ListBackupsResponse {
            backups: listed.items.iter().map(backup_to_pb).collect(),
            page: self.next_page(listed.next, IdKind::Backup)?.into(),
            ..Default::default()
        })
    }
}
