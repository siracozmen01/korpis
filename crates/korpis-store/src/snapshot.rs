// SPDX-License-Identifier: AGPL-3.0-or-later

//! Snapshots (§4 of docs/06-storage.md), kept from what nodes journal.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{SnapshotRead, SnapshotWrite, StoreResult};
use korpis_core::workload::volume::{ReportedSnapshot, Snapshot, SnapshotEvent};
use std::time::SystemTime;
use uuid::Uuid;

/// One stored snapshot.
struct SnapshotRow {
    id: Uuid,
    volume_id: Uuid,
    name: String,
    created_at: chrono::DateTime<chrono::Utc>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl SnapshotRow {
    fn into_domain(self) -> Snapshot {
        Snapshot {
            id: self.id,
            volume: self.volume_id,
            name: self.name,
            created: to_time(self.created_at),
            deleted_at: self.deleted_at.map(to_time),
        }
    }
}

impl SnapshotRead for PgStore {
    async fn snapshot(&self, id: Uuid) -> StoreResult<Option<Snapshot>> {
        let row = sqlx::query_as!(
            SnapshotRow,
            "SELECT id, volume_id, name, created_at, deleted_at FROM snapshot WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(SnapshotRow::into_domain))
    }

    async fn snapshots(&self, volume: Uuid, deleted: bool) -> StoreResult<Vec<Snapshot>> {
        let rows = sqlx::query_as!(
            SnapshotRow,
            "SELECT id, volume_id, name, created_at, deleted_at FROM snapshot
              WHERE volume_id = $1 AND ($2 OR deleted_at IS NULL)
              ORDER BY id",
            volume,
            deleted
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(SnapshotRow::into_domain).collect())
    }
}

impl SnapshotWrite for PgTx {
    async fn record_snapshot(
        &mut self,
        node: Uuid,
        s: &ReportedSnapshot,
        now: SystemTime,
    ) -> StoreResult<bool> {
        let Some(volume) = sqlx::query_scalar!(
            "SELECT id FROM volume WHERE node_id = $1 AND helper_id = $2 AND deleted_at IS NULL",
            node,
            s.volume
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        else {
            return Ok(false);
        };
        match s.event {
            SnapshotEvent::Taken => {
                sqlx::query!(
                    "INSERT INTO snapshot (id, volume_id, name, created_at)
                     VALUES ($1, $2, $3, $4) ON CONFLICT (id) DO NOTHING",
                    s.snapshot,
                    volume,
                    s.name,
                    from_time(now)
                )
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
            }
            SnapshotEvent::Deleted => {
                sqlx::query!(
                    "UPDATE snapshot SET deleted_at = $3
                      WHERE id = $1 AND volume_id = $2 AND deleted_at IS NULL",
                    s.snapshot,
                    volume,
                    from_time(now)
                )
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
            }
            SnapshotEvent::RolledBack => {}
        }
        Ok(true)
    }
}
