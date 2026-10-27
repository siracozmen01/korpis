// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes (§7 of docs/06-storage.md), kept from what nodes journal.

use crate::errors::from_sqlx;
use crate::replica::with_replicas;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{StoreError, StoreResult, VolumeRead, VolumeWrite};
use korpis_core::id::{Id, IdKind};
use korpis_core::workload::stream::StreamKind;
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
    store: Option<String>,
    helper_id: String,
    storage_class: String,
    size_bytes: i64,
    quota_enforced: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    detached_at: Option<chrono::DateTime<chrono::Utc>>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    snapshots: Vec<Uuid>,
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
            node: if self.store.is_some() { None } else { Some(self.node_id) },
            store: self.store,
            reported_by: self.node_id,
            helper_id: self.helper_id,
            storage_class: self.storage_class,
            size_bytes,
            quota_enforced: self.quota_enforced,
            created: to_time(self.created_at),
            detached_at: self.detached_at.map(to_time),
            deleted_at: self.deleted_at.map(to_time),
            snapshots: self.snapshots,
            replicas: Vec::new(),
            divergent: Vec::new(),
            divergent_of: None,
        })
    }
}

impl VolumeRead for PgStore {
    async fn volume(&self, id: Uuid) -> StoreResult<Option<Volume>> {
        let row = sqlx::query_as!(
            VolumeRow,
            r#"SELECT id, project_id, created_by, attached_to, name, kind, node_id, store, helper_id,
                      storage_class, size_bytes, quota_enforced, created_at, detached_at,
                      deleted_at,
                      ARRAY(SELECT s.id FROM snapshot s
                             WHERE s.volume_id = volume.id AND s.deleted_at IS NULL
                             ORDER BY s.id) AS "snapshots!"
                 FROM volume
                WHERE id = $1"#,
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        let mut found: Vec<Volume> =
            row.map(VolumeRow::into_domain).transpose()?.into_iter().collect();
        with_replicas(&self.pool, &mut found).await?;
        Ok(found.pop())
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
            r#"SELECT id, project_id, created_by, attached_to, name, kind, node_id, store, helper_id,
                      storage_class, size_bytes, quota_enforced, created_at, detached_at,
                      deleted_at,
                      ARRAY(SELECT s.id FROM snapshot s
                             WHERE s.volume_id = volume.id AND s.deleted_at IS NULL
                             ORDER BY s.id) AS "snapshots!"
                 FROM volume
                WHERE project_id = $1
                  AND ($2::uuid IS NULL OR created_by = $2 OR attached_to = $2)
                  AND ($3 OR deleted_at IS NULL)
                  AND ($4::uuid IS NULL OR id > $4)
                ORDER BY id LIMIT $5"#,
            project,
            workload,
            deleted,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        let mut found =
            rows.into_iter().map(VolumeRow::into_domain).collect::<StoreResult<Vec<_>>>()?;
        with_replicas(&self.pool, &mut found).await?;
        Ok(found)
    }

    async fn replicable(&self, page: Page) -> StoreResult<Vec<Volume>> {
        let rows = sqlx::query_as!(
            VolumeRow,
            r#"SELECT id, project_id, created_by, attached_to, name, kind, node_id, store, helper_id,
                      storage_class, size_bytes, quota_enforced, created_at, detached_at,
                      deleted_at,
                      ARRAY(SELECT s.id FROM snapshot s
                             WHERE s.volume_id = volume.id AND s.deleted_at IS NULL
                             ORDER BY s.id) AS "snapshots!"
                 FROM volume
                WHERE deleted_at IS NULL AND store IS NULL AND kind = 'declared'
                  AND (attached_to IS NOT NULL
                       OR EXISTS (SELECT 1 FROM volume_replica r WHERE r.volume_id = volume.id))
                  AND ($1::uuid IS NULL OR id > $1)
                ORDER BY id LIMIT $2"#,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        let mut found =
            rows.into_iter().map(VolumeRow::into_domain).collect::<StoreResult<Vec<_>>>()?;
        with_replicas(&self.pool, &mut found).await?;
        Ok(found)
    }
}

impl VolumeWrite for PgTx {
    async fn claim_volume(&mut self, volume: Uuid, workload: Uuid) -> StoreResult<bool> {
        let claimed = sqlx::query!(
            "UPDATE volume SET attached_to = $2, detached_at = NULL
              WHERE id = $1 AND deleted_at IS NULL
                AND (attached_to IS NULL OR attached_to = $2)",
            volume,
            workload
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(claimed.rows_affected() == 1)
    }

    async fn record_volume(
        &mut self,
        node: Uuid,
        v: &ReportedVolume,
        store: Option<&str>,
        now: SystemTime,
    ) -> StoreResult<bool> {
        match (v.event, store) {
            (VolumeEvent::Ensured, None) => ensured(self, node, v, now).await.map(|()| true),
            (VolumeEvent::Ensured, Some(store)) => {
                shared_ensured(self, (node, store), v, now).await
            }
            (VolumeEvent::Deleted, Some(store)) => shared_deleted(self, store, v, now).await,
            (VolumeEvent::Deleted, None) => {
                if v.kind == VolumeKind::Stream {
                    stream_gone(self, node, v.workload, run_of(&v.name)).await?;
                }
                // Its snapshots were deleted with it.
                sqlx::query!(
                    "UPDATE snapshot SET deleted_at = $3
                      WHERE deleted_at IS NULL
                        AND volume_id IN (SELECT id FROM volume
                                           WHERE node_id = $1 AND helper_id = $2
                                             AND deleted_at IS NULL AND store IS NULL)",
                    node,
                    v.helper_id,
                    from_time(now)
                )
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
                sqlx::query!(
                    "UPDATE volume SET deleted_at = $3, attached_to = NULL,
                                       detached_at = coalesce(detached_at, $3)
                      WHERE node_id = $1 AND helper_id = $2 AND deleted_at IS NULL
                        AND store IS NULL",
                    node,
                    v.helper_id,
                    from_time(now)
                )
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
                Ok(true)
            }
        }
    }
}

/// The run whose stream the volume named `name` holds, for a run's stream volume.
fn run_of(name: &str) -> Option<Uuid> {
    let (_, run) = StreamKind::of_run_volume(name)?;
    Id::parse(IdKind::Run, run).ok().map(|id| id.uuid())
}

/// A console stream's volume deleted on `node`, the workload's or one run's: no segment of the
/// stream is held there any more, so what was not offloaded is lost, as its index now says.
async fn stream_gone(
    tx: &mut PgTx,
    node: Uuid,
    workload: Uuid,
    run: Option<Uuid>,
) -> StoreResult<()> {
    sqlx::query!(
        "UPDATE stream_segment SET node_id = NULL
          WHERE node_id = $1
            AND stream_id IN (SELECT id FROM stream
                               WHERE workload_id = $2 AND kind = 'console'
                                 AND run_id IS NOT DISTINCT FROM $3)",
        node,
        workload,
        run
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

/// A volume ensured: created on its first report, and on a later one attached to the workload
/// that ensured it, which may be one adopting it, with what the node now says of it. An adopting
/// workload asks for no size, so a zero keeps the size it has.
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
         ON CONFLICT (node_id, helper_id) WHERE deleted_at IS NULL AND store IS NULL DO UPDATE
            SET attached_to = EXCLUDED.attached_to,
                detached_at = NULL,
                storage_class = EXCLUDED.storage_class,
                size_bytes = CASE WHEN EXCLUDED.size_bytes > 0 THEN EXCLUDED.size_bytes
                                  ELSE volume.size_bytes END,
                quota_enforced = EXCLUDED.quota_enforced,
                promoting = NULL",
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

/// A volume in a shared store ensured by `node`: as on a node's disk, but known by its store and
/// the helper's identifier there, whichever node reports it, and kept by the node that last did.
/// One already there is the reporting workload's only if it is attached to it, which an adopting
/// workload's is from its approval; any other is left as it is, and the report refused, since a
/// node's word names the store (Finding 201 of docs/23-walkthroughs.md). A volume of a block
/// class takes the identifier of the device made for it (§2.1 of docs/06-storage.md). Whether it
/// was kept.
async fn shared_ensured(
    tx: &mut PgTx,
    (node, store): (Uuid, &str),
    v: &ReportedVolume,
    now: SystemTime,
) -> StoreResult<bool> {
    let size = i64::try_from(v.size_bytes)
        .map_err(|_| StoreError::Corrupt(format!("volume size {}", v.size_bytes)))?;
    let done = sqlx::query!(
        "INSERT INTO volume (id, project_id, created_by, attached_to, name, kind, node_id, store,
                             helper_id, storage_class, size_bytes, quota_enforced, created_at)
         SELECT coalesce((SELECT b.id FROM block_device b
                           WHERE b.store = $6 AND b.helper_id = $7 AND b.deleted_at IS NULL),
                         $1),
                w.project_id, w.id, w.id, $3, $4, $5, $6, $7, $8, $9, $10, $11
           FROM workload w WHERE w.id = $2
         ON CONFLICT (store, helper_id) WHERE deleted_at IS NULL AND store IS NOT NULL DO UPDATE
            SET attached_to = EXCLUDED.attached_to,
                detached_at = NULL,
                node_id = EXCLUDED.node_id,
                storage_class = EXCLUDED.storage_class,
                size_bytes = CASE WHEN EXCLUDED.size_bytes > 0 THEN EXCLUDED.size_bytes
                                  ELSE volume.size_bytes END,
                quota_enforced = EXCLUDED.quota_enforced
          WHERE volume.project_id = EXCLUDED.project_id
            AND volume.attached_to = EXCLUDED.attached_to",
        Uuid::now_v7(),
        v.workload,
        v.name,
        v.kind.as_str(),
        node,
        store,
        v.helper_id,
        v.storage_class,
        size,
        v.quota_enforced,
        from_time(now)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(done.rows_affected() == 1)
}

/// A volume in a shared store deleted, by whichever node reached it: only the reporting
/// workload's, attached to it or, retained, made for it, whose plan deleted it; whether it was.
async fn shared_deleted(
    tx: &mut PgTx,
    store: &str,
    v: &ReportedVolume,
    now: SystemTime,
) -> StoreResult<bool> {
    let done = sqlx::query!(
        "UPDATE volume SET deleted_at = $3, attached_to = NULL,
                           detached_at = coalesce(detached_at, $3)
          WHERE store = $1 AND helper_id = $2 AND deleted_at IS NULL
            AND (attached_to = $4 OR (attached_to IS NULL AND created_by = $4))",
        store,
        v.helper_id,
        from_time(now),
        v.workload
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(done.rows_affected() == 1)
}
