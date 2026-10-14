// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes (§7 of docs/06-storage.md), kept from what nodes journal.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{StoreError, StoreResult, VolumeRead, VolumeWrite};
use korpis_core::workload::volume::{ReportedVolume, Volume, VolumeEvent, VolumeKind};
use std::time::SystemTime;
use uuid::Uuid;

/// One stored volume.
struct VolumeRow {
    id: Uuid,
    project_id: Uuid,
    created_by: Uuid,
    attached_to: Option<Uuid>,
    name: String,
    kind: String,
    node_id: Uuid,
    helper_id: String,
    storage_class: String,
    size_bytes: i64,
    quota_enforced: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    detached_at: Option<chrono::DateTime<chrono::Utc>>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl VolumeRow {
    fn into_domain(self) -> StoreResult<Volume> {
        let kind = VolumeKind::parse(&self.kind)
            .ok_or_else(|| StoreError::Corrupt(format!("volume kind {}", self.kind)))?;
        let size_bytes = u64::try_from(self.size_bytes)
            .map_err(|_| StoreError::Corrupt(format!("volume size {}", self.size_bytes)))?;
        Ok(Volume {
            id: self.id,
            project: self.project_id,
            created_by: self.created_by,
            attached_to: self.attached_to,
            name: self.name,
            kind,
            node: self.node_id,
            helper_id: self.helper_id,
            storage_class: self.storage_class,
            size_bytes,
            quota_enforced: self.quota_enforced,
            created: to_time(self.created_at),
            detached_at: self.detached_at.map(to_time),
            deleted_at: self.deleted_at.map(to_time),
        })
    }
}

impl VolumeRead for PgStore {
    async fn volume(&self, id: Uuid) -> StoreResult<Option<Volume>> {
        let row = sqlx::query_as!(
            VolumeRow,
            "SELECT id, project_id, created_by, attached_to, name, kind, node_id, helper_id,
                    storage_class, size_bytes, quota_enforced, created_at, detached_at, deleted_at
               FROM volume WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(VolumeRow::into_domain).transpose()
    }

    async fn volumes(
        &self,
        project: Uuid,
        workload: Option<Uuid>,
        deleted: bool,
        page: Page,
    ) -> StoreResult<Vec<Volume>> {
        let rows = sqlx::query_as!(
            VolumeRow,
            "SELECT id, project_id, created_by, attached_to, name, kind, node_id, helper_id,
                    storage_class, size_bytes, quota_enforced, created_at, detached_at, deleted_at
               FROM volume
              WHERE project_id = $1
                AND ($2::uuid IS NULL OR created_by = $2 OR attached_to = $2)
                AND ($3 OR deleted_at IS NULL)
                AND ($4::uuid IS NULL OR id > $4)
              ORDER BY id LIMIT $5",
            project,
            workload,
            deleted,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(VolumeRow::into_domain).collect()
    }
}

impl VolumeWrite for PgTx {
    async fn record_volume(
        &mut self,
        node: Uuid,
        v: &ReportedVolume,
        now: SystemTime,
    ) -> StoreResult<()> {
        match v.event {
            VolumeEvent::Ensured => ensured(self, node, v, now).await,
            VolumeEvent::Deleted => {
                sqlx::query!(
                    "UPDATE volume SET deleted_at = $3, attached_to = NULL,
                                       detached_at = coalesce(detached_at, $3)
                      WHERE node_id = $1 AND helper_id = $2 AND deleted_at IS NULL",
                    node,
                    v.helper_id,
                    from_time(now)
                )
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
                Ok(())
            }
        }
    }
}

/// A volume ensured: created on its first report, and on a later one attached to the workload
/// that ensured it, which may be one adopting it, with what the node now says of it.
async fn ensured(
    tx: &mut PgTx,
    node: Uuid,
    v: &ReportedVolume,
    now: SystemTime,
) -> StoreResult<()> {
    let size = i64::try_from(v.size_bytes)
        .map_err(|_| StoreError::Corrupt(format!("volume size {}", v.size_bytes)))?;
    sqlx::query!(
        "INSERT INTO volume (id, project_id, created_by, attached_to, name, kind, node_id,
                             helper_id, storage_class, size_bytes, quota_enforced, created_at)
         SELECT $1, w.project_id, w.id, w.id, $3, $4, $5, $6, $7, $8, $9, $10
           FROM workload w WHERE w.id = $2
         ON CONFLICT (node_id, helper_id) WHERE deleted_at IS NULL DO UPDATE
            SET attached_to = EXCLUDED.attached_to,
                detached_at = NULL,
                storage_class = EXCLUDED.storage_class,
                size_bytes = EXCLUDED.size_bytes,
                quota_enforced = EXCLUDED.quota_enforced",
        Uuid::now_v7(),
        v.workload,
        v.name,
        v.kind.as_str(),
        node,
        v.helper_id,
        v.storage_class,
        size,
        v.quota_enforced,
        from_time(now)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}
