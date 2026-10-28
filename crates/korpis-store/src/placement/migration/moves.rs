// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a migration's sides report, its phases, and its placement's move (migration 0146, §8.1 of
//! docs/05-scheduling.md). A volume's progress is kept by its name in the workload's declaration,
//! which is what a node knows it by; where both sides' manifests are in, the first path where
//! they differ is kept beside them.

use super::volumes_of;
use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::from_time;
use korpis_app::ports::{MigrationMoves, StoreResult};
use korpis_core::workload::migration::{
    Chunk, MigrationVolume, Phase, Side, VolumeProgress, Watermark, first_difference,
};
use serde_json::Value;
use std::time::SystemTime;
use uuid::Uuid;

fn chunks_doc(chunks: &[Chunk]) -> Option<Value> {
    (!chunks.is_empty()).then(|| {
        Value::Array(
            chunks
                .iter()
                .map(|c| serde_json::json!({ "first_path": c.first_path, "digest": c.digest }))
                .collect(),
        )
    })
}

fn chunks_of(doc: Option<&Value>) -> Vec<Chunk> {
    let Some(Value::Array(runs)) = doc else { return Vec::new() };
    runs.iter()
        .filter_map(|r| {
            Some(Chunk {
                first_path: r.get("first_path")?.as_str()?.to_owned(),
                digest: r.get("digest")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

fn count(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

impl PgTx {
    /// Records `v`, `side`'s report of one volume of the migration `id`.
    async fn one_volume(&mut self, id: Uuid, side: Side, v: &VolumeProgress) -> StoreResult<()> {
        let chunks = chunks_doc(&v.chunks);
        match side {
            Side::Source => sqlx::query!(
                "UPDATE migration_volume m
                    SET passes = m.passes + CASE WHEN $3::uuid IS DISTINCT FROM m.pass
                                                 AND $3::uuid IS NOT NULL THEN 1 ELSE 0 END,
                        pass = coalesce($3, m.pass),
                        sent_bytes = coalesce($4, m.sent_bytes),
                        source_manifest = coalesce($5, m.source_manifest),
                        source_chunks = coalesce($6, m.source_chunks)
                   FROM volume v
                  WHERE m.migration_id = $1 AND v.id = m.volume_id AND v.name = $2",
                id,
                v.name,
                v.pass,
                v.sent_bytes.map(count),
                v.manifest,
                chunks
            )
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?,
            Side::Destination => sqlx::query!(
                "UPDATE migration_volume m
                    SET held_pass = CASE WHEN $3 THEN coalesce($4, m.held_pass) ELSE m.held_pass END,
                        destination_helper_id = coalesce($5, m.destination_helper_id),
                        destination_manifest = coalesce($6, m.destination_manifest),
                        destination_chunks = coalesce($7, m.destination_chunks)
                   FROM volume v
                  WHERE m.migration_id = $1 AND v.id = m.volume_id AND v.name = $2",
                id,
                v.name,
                v.held,
                v.pass,
                v.helper_id,
                v.manifest,
                chunks
            )
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?,
        };
        Ok(())
    }

    /// Records where the two manifests of each volume of the migration `id` first differ.
    async fn differences(&mut self, id: Uuid) -> StoreResult<()> {
        let rows = sqlx::query!(
            "SELECT volume_id, source_chunks, destination_chunks FROM migration_volume
              WHERE migration_id = $1 AND source_manifest IS NOT NULL
                AND destination_manifest IS NOT NULL
                AND source_manifest <> destination_manifest",
            id
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        for r in rows {
            let (s, d) =
                (chunks_of(r.source_chunks.as_ref()), chunks_of(r.destination_chunks.as_ref()));
            let at = first_difference(&s, &d).unwrap_or_else(|| "the manifests' digests".into());
            sqlx::query!(
                "UPDATE migration_volume SET first_difference = $3
                  WHERE migration_id = $1 AND volume_id = $2",
                id,
                r.volume_id,
                at
            )
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        }
        Ok(())
    }
}

impl MigrationMoves for PgTx {
    async fn migration_volumes_in(&mut self, id: Uuid) -> StoreResult<Vec<MigrationVolume>> {
        let rows = volumes_of(&mut *self.tx, &[id]).await?;
        rows.into_iter().map(super::VolumeRow::into_domain).collect()
    }

    async fn record_volume_progress(
        &mut self,
        id: Uuid,
        side: Side,
        volumes: &[VolumeProgress],
    ) -> StoreResult<()> {
        for v in volumes {
            self.one_volume(id, side, v).await?;
        }
        self.differences(id).await
    }

    async fn record_watermarks(&mut self, id: Uuid, watermarks: &[Watermark]) -> StoreResult<()> {
        for w in watermarks {
            sqlx::query!(
                "INSERT INTO migration_stream (migration_id, stream, next_offset, journaled_to)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (migration_id, stream) DO UPDATE
                    SET next_offset = EXCLUDED.next_offset, journaled_to = EXCLUDED.journaled_to",
                id,
                w.stream,
                count(w.next_offset),
                count(w.journaled_to)
            )
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        }
        Ok(())
    }

    async fn record_reported(&mut self, id: Uuid, side: Side, phase: Phase) -> StoreResult<()> {
        let (source, destination) = match side {
            Side::Source => (Some(phase.as_str()), None),
            Side::Destination => (None, Some(phase.as_str())),
        };
        sqlx::query!(
            "UPDATE migration SET source_done = coalesce($2, source_done),
                                  destination_done = coalesce($3, destination_done)
              WHERE id = $1",
            id,
            source,
            destination
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn advance_migration(
        &mut self,
        id: Uuid,
        phase: Phase,
        (at, hold_until): (SystemTime, Option<SystemTime>),
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE migration SET phase = $2, phase_since = $3,
                                  hold_until = coalesce($4, hold_until)
              WHERE id = $1 AND outcome IS NULL",
            id,
            phase.as_str(),
            from_time(at),
            hold_until.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn move_placement(
        &mut self,
        (id, workload): (Uuid, Uuid),
        (from, to): (Uuid, Uuid),
        volumes: &[(Uuid, String)],
        at: SystemTime,
    ) -> StoreResult<()> {
        // One authoritative placement at every instant: the old one yields first.
        sqlx::query!(
            "UPDATE placement SET authoritative = false WHERE workload_id = $1 AND node_id = $2",
            workload,
            from
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        sqlx::query!(
            "UPDATE placement SET authoritative = true, bound_at = $3, bound_by = 'migration'
              WHERE workload_id = $1 AND node_id = $2",
            workload,
            to,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        for (volume, helper_id) in volumes {
            sqlx::query!(
                "UPDATE volume SET node_id = $2, helper_id = $3 WHERE id = $1",
                volume,
                to,
                helper_id
            )
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        }
        sqlx::query!(
            "UPDATE migration SET moved_at = CASE WHEN destination_id = $2 THEN $3::timestamptz END
              WHERE id = $1",
            id,
            to,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn keep_divergent(
        &mut self,
        volume: Uuid,
        (node, helper_id): (Uuid, &str),
        (since, held_until): (SystemTime, SystemTime),
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO volume (id, project_id, created_by, attached_to, name, kind, node_id,
                                 helper_id, storage_class, size_bytes, quota_enforced, created_at,
                                 detached_at, divergent_of, divergent_since, held_until)
             SELECT $2, project_id, created_by, NULL, name, kind, $3, $4, storage_class,
                    size_bytes, quota_enforced, created_at, $5, id, $5, $6
               FROM volume WHERE id = $1",
            volume,
            Uuid::now_v7(),
            node,
            helper_id,
            from_time(since),
            from_time(held_until)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
