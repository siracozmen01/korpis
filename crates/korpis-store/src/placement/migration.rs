// SPDX-License-Identifier: AGPL-3.0-or-later

//! Migrations (migration 0145, §8.1 of docs/05-scheduling.md): one row each, a row for each of
//! its volumes, and each stream's watermark once the source reported it; what each side reported
//! and the placement's move are migration 0146's.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{MigrationRead, MigrationWrite, StoreError, StoreResult};
use korpis_core::workload::migration::{
    Crossing, Migration, MigrationVolume, Outcome, Phase, Reported, Watermark,
};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

mod moves;

type Time = chrono::DateTime<chrono::Utc>;

/// One stored migration.
struct MigrationRow {
    id: Uuid,
    plan_id: Uuid,
    workload_id: Uuid,
    source_id: Uuid,
    destination_id: Uuid,
    phase: String,
    phase_since: Time,
    hold_secs: i64,
    hold_until: Option<Time>,
    outcome: Option<String>,
    reason: Option<String>,
    made_at: Time,
    source_done: Option<String>,
    destination_done: Option<String>,
    moved_at: Option<Time>,
}

impl MigrationRow {
    fn into_domain(self) -> StoreResult<Migration> {
        let corrupt = |what: &str, v: &str| StoreError::Corrupt(format!("migration {what} {v:?}"));
        let phase = Phase::parse(&self.phase).ok_or_else(|| corrupt("phase", &self.phase))?;
        let outcome = match self.outcome {
            Some(o) => {
                let parsed = Outcome::parse(&o).ok_or_else(|| corrupt("outcome", &o))?;
                Some((parsed, self.reason.unwrap_or_default()))
            }
            None => None,
        };
        let done = |p: Option<String>| match p {
            Some(p) => Phase::parse(&p).map(Some).ok_or_else(|| corrupt("report", &p)),
            None => Ok(None),
        };
        let reported =
            Reported { source: done(self.source_done)?, destination: done(self.destination_done)? };
        Ok(Migration {
            id: self.id,
            plan: self.plan_id,
            workload: self.workload_id,
            source: self.source_id,
            destination: self.destination_id,
            phase,
            phase_since: to_time(self.phase_since),
            hold: Duration::from_secs(u64::try_from(self.hold_secs).unwrap_or(0)),
            hold_until: self.hold_until.map(to_time),
            outcome,
            made_at: to_time(self.made_at),
            reported,
            moved_at: self.moved_at.map(to_time),
        })
    }
}

/// One stored volume of a migration, with its name in the workload's declaration.
pub(super) struct VolumeRow {
    migration_id: Uuid,
    volume_id: Uuid,
    name: String,
    crossing: String,
    size_bytes: i64,
    source_helper_id: String,
    passes: i32,
    pass: Option<Uuid>,
    pass_since: Option<Time>,
    parent_pass: Option<Uuid>,
    sent_bytes: Option<i64>,
    held_pass: Option<Uuid>,
    destination_helper_id: Option<String>,
    source_manifest: Option<String>,
    destination_manifest: Option<String>,
    first_difference: Option<String>,
}

impl VolumeRow {
    pub(super) fn into_domain(self) -> StoreResult<MigrationVolume> {
        let crossing = Crossing::parse(&self.crossing).ok_or_else(|| {
            StoreError::Corrupt(format!("migration crossing {:?}", self.crossing))
        })?;
        let count = |n: i64| u64::try_from(n).unwrap_or(0);
        Ok(MigrationVolume {
            volume: self.volume_id,
            name: self.name,
            crossing,
            size_bytes: count(self.size_bytes),
            source_helper_id: self.source_helper_id,
            passes: u32::try_from(self.passes).unwrap_or(0),
            pass: self.pass.zip(self.pass_since.map(to_time)),
            parent_pass: self.parent_pass,
            sent_bytes: self.sent_bytes.map(count),
            held_pass: self.held_pass,
            destination_helper_id: self.destination_helper_id,
            manifests: (self.source_manifest, self.destination_manifest),
            first_difference: self.first_difference,
        })
    }
}

/// Each of `rows` with the volumes of `volumes` that are its own.
fn joined(
    rows: Vec<MigrationRow>,
    volumes: Vec<VolumeRow>,
) -> StoreResult<Vec<(Migration, Vec<MigrationVolume>)>> {
    let mut out: Vec<(Migration, Vec<MigrationVolume>)> = rows
        .into_iter()
        .map(|r| r.into_domain().map(|m| (m, Vec::new())))
        .collect::<Result<_, _>>()?;
    for v in volumes {
        if let Some((_, vs)) = out.iter_mut().find(|(m, _)| m.id == v.migration_id) {
            vs.push(v.into_domain()?);
        }
    }
    Ok(out)
}

pub(super) async fn volumes_of<'e, E: sqlx::PgExecutor<'e>>(
    pool: E,
    ids: &[Uuid],
) -> StoreResult<Vec<VolumeRow>> {
    sqlx::query_as!(
        VolumeRow,
        "SELECT m.migration_id, m.volume_id, v.name, m.crossing, m.size_bytes,
                m.source_helper_id, m.passes, m.pass, m.pass_since, m.parent_pass, m.sent_bytes,
                m.held_pass,
                m.destination_helper_id, m.source_manifest, m.destination_manifest,
                m.first_difference
           FROM migration_volume m JOIN volume v ON v.id = m.volume_id
          WHERE m.migration_id = ANY($1)
          ORDER BY m.migration_id, v.name",
        ids
    )
    .fetch_all(pool)
    .await
    .map_err(from_sqlx)
}

impl MigrationRead for PgStore {
    async fn migrations_of(
        &self,
        workload: Uuid,
    ) -> StoreResult<Vec<(Migration, Vec<MigrationVolume>)>> {
        let rows = sqlx::query_as!(
            MigrationRow,
            "SELECT id, plan_id, workload_id, source_id, destination_id, phase, phase_since,
                    hold_secs, hold_until, outcome, reason, made_at, source_done,
                    destination_done, moved_at
               FROM migration WHERE workload_id = $1 ORDER BY made_at DESC, id DESC",
            workload
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        joined(rows, volumes_of(&self.pool, &ids).await?)
    }

    async fn migration(&self, id: Uuid) -> StoreResult<Option<(Migration, Vec<MigrationVolume>)>> {
        let rows = sqlx::query_as!(
            MigrationRow,
            "SELECT id, plan_id, workload_id, source_id, destination_id, phase, phase_since,
                    hold_secs, hold_until, outcome, reason, made_at, source_done,
                    destination_done, moved_at
               FROM migration WHERE id = $1",
            id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(joined(rows, volumes_of(&self.pool, &[id]).await?)?.into_iter().next())
    }

    async fn migrations_on(
        &self,
        node: Uuid,
    ) -> StoreResult<Vec<(Migration, Vec<MigrationVolume>, Vec<Watermark>)>> {
        let rows = sqlx::query_as!(
            MigrationRow,
            "SELECT id, plan_id, workload_id, source_id, destination_id, phase, phase_since,
                    hold_secs, hold_until, outcome, reason, made_at, source_done,
                    destination_done, moved_at
               FROM migration
              WHERE outcome IS NULL AND (source_id = $1 OR destination_id = $1)
              ORDER BY workload_id",
            node
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        let marks = sqlx::query!(
            "SELECT migration_id, stream, next_offset, journaled_to FROM migration_stream
              WHERE migration_id = ANY($1) ORDER BY stream",
            &ids
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        let count = |n: i64| u64::try_from(n).unwrap_or(0);
        Ok(joined(rows, volumes_of(&self.pool, &ids).await?)?
            .into_iter()
            .map(|(m, volumes)| {
                let own = marks.iter().filter(|w| w.migration_id == m.id);
                let watermarks = own
                    .map(|w| Watermark {
                        stream: w.stream.clone(),
                        next_offset: count(w.next_offset),
                        journaled_to: count(w.journaled_to),
                    })
                    .collect();
                (m, volumes, watermarks)
            })
            .collect())
    }

    async fn live_migrations(&self) -> StoreResult<Vec<Migration>> {
        let rows = sqlx::query_as!(
            MigrationRow,
            "SELECT id, plan_id, workload_id, source_id, destination_id, phase, phase_since,
                    hold_secs, hold_until, outcome, reason, made_at, source_done,
                    destination_done, moved_at
               FROM migration
              WHERE outcome IS NULL ORDER BY workload_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(MigrationRow::into_domain).collect()
    }
}

impl MigrationWrite for PgTx {
    async fn insert_migration(
        &mut self,
        m: &Migration,
        volumes: &[MigrationVolume],
    ) -> StoreResult<()> {
        let hold = i64::try_from(m.hold.as_secs()).unwrap_or(i64::MAX);
        sqlx::query!(
            "INSERT INTO migration (id, plan_id, workload_id, source_id, destination_id, phase,
                                    phase_since, hold_secs, made_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            m.id,
            m.plan,
            m.workload,
            m.source,
            m.destination,
            m.phase.as_str(),
            from_time(m.phase_since),
            hold,
            from_time(m.made_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        for v in volumes {
            sqlx::query!(
                "INSERT INTO migration_volume (migration_id, volume_id, crossing, size_bytes,
                                               source_helper_id)
                 VALUES ($1, $2, $3, $4, $5)",
                m.id,
                v.volume,
                v.crossing.as_str(),
                i64::try_from(v.size_bytes).unwrap_or(i64::MAX),
                v.source_helper_id
            )
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        }
        Ok(())
    }

    async fn live_migration_in(&mut self, workload: Uuid) -> StoreResult<Option<Migration>> {
        let row = sqlx::query_as!(
            MigrationRow,
            "SELECT id, plan_id, workload_id, source_id, destination_id, phase, phase_since,
                    hold_secs, hold_until, outcome, reason, made_at, source_done,
                    destination_done, moved_at
               FROM migration WHERE workload_id = $1 AND outcome IS NULL FOR UPDATE",
            workload
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(MigrationRow::into_domain).transpose()
    }

    async fn migration_in(&mut self, id: Uuid) -> StoreResult<Option<Migration>> {
        let row = sqlx::query_as!(
            MigrationRow,
            "SELECT id, plan_id, workload_id, source_id, destination_id, phase, phase_since,
                    hold_secs, hold_until, outcome, reason, made_at, source_done,
                    destination_done, moved_at
               FROM migration WHERE id = $1 FOR UPDATE",
            id
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(MigrationRow::into_domain).transpose()
    }

    async fn free_placement(&mut self, workload: Uuid, node: Uuid) -> StoreResult<bool> {
        let done = sqlx::query!(
            "DELETE FROM placement WHERE workload_id = $1 AND node_id = $2 AND NOT authoritative",
            workload,
            node
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn end_migration(
        &mut self,
        id: Uuid,
        (outcome, reason): (Outcome, &str),
        at: SystemTime,
    ) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE migration SET outcome = $2, reason = $3, ended_at = $4
              WHERE id = $1 AND outcome IS NULL",
            id,
            outcome.as_str(),
            reason,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
