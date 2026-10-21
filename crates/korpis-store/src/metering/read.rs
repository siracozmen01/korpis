// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading metering: what is overdue a gap, a workload's records, the records of a scope a page
//! at a time, and the days closed.

use super::{counters, losses, read};
use crate::PgStore;
use crate::errors::from_sqlx;
use crate::stored::{count, from_time, to_time};
use chrono::{DateTime, Utc};
use korpis_app::ports::{MeteringRead, Overdue, StoreError, StoreResult, UsageQuery};
use korpis_core::metering::{Record, RecordKind};
use std::time::SystemTime;
use uuid::Uuid;

/// A record as its row holds it.
struct RecordRow {
    id: Uuid,
    workload_id: Uuid,
    node_id: Option<Uuid>,
    lease_epoch: i64,
    interval_start: DateTime<Utc>,
    interval_end: DateTime<Utc>,
    kind: String,
    cause: Option<String>,
    supersedes: Vec<Uuid>,
    cpu_usec: Option<i64>,
    cpu_throttled_usec: Option<i64>,
    io_read_bytes: Option<i64>,
    io_write_bytes: Option<i64>,
    net_rx_bytes: Option<i64>,
    net_tx_bytes: Option<i64>,
    memory_kib_seconds: Option<i64>,
    memory_peak_bytes: Option<i64>,
    disk_bytes: Option<i64>,
    losses: serde_json::Value,
    recorded_at: DateTime<Utc>,
}

impl RecordRow {
    fn into_domain(self) -> StoreResult<Record> {
        let kind = RecordKind::parse(&self.kind)
            .ok_or_else(|| StoreError::Corrupt(format!("metering kind {}", self.kind)))?;
        let used = [
            self.cpu_usec,
            self.cpu_throttled_usec,
            self.io_read_bytes,
            self.io_write_bytes,
            self.net_rx_bytes,
            self.net_tx_bytes,
            self.memory_kib_seconds,
        ];
        Ok(Record {
            id: self.id,
            workload: self.workload_id,
            node: self.node_id,
            lease_epoch: count("lease epoch", self.lease_epoch)?,
            from: to_time(self.interval_start),
            to: to_time(self.interval_end),
            kind,
            cause: self.cause,
            supersedes: self.supersedes,
            used: counters(used)?,
            memory_peak_bytes: read("memory_peak_bytes", self.memory_peak_bytes)?,
            disk_bytes: read("disk_bytes", self.disk_bytes)?,
            losses: losses(self.losses)?,
            recorded_at: Some(to_time(self.recorded_at)),
        })
    }
}

impl MeteringRead for PgStore {
    async fn metering_overdue(&self, cutoff: SystemTime) -> StoreResult<Vec<Overdue>> {
        let rows = sqlx::query!(
            r#"SELECT p.workload_id AS "workload_id!", p.node_id AS "node_id!", c.at AS "from!"
                 FROM placement p
                 LEFT JOIN metering_reading r
                        ON r.workload_id = p.workload_id AND r.node_id = p.node_id
                 CROSS JOIN LATERAL (SELECT CASE WHEN r.workload_id IS NULL THEN p.bound_at
                      ELSE greatest(r.interval_end, coalesce(r.gapped_through, r.interval_end))
                      END AS at) c
                WHERE c.at < $1"#,
            from_time(cutoff)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows
            .into_iter()
            .map(|r| Overdue { workload: r.workload_id, node: r.node_id, from: to_time(r.from) })
            .collect())
    }

    async fn metering_records(
        &self,
        workload: Uuid,
        from: SystemTime,
        to: SystemTime,
    ) -> StoreResult<Vec<Record>> {
        let rows = sqlx::query_as!(
            RecordRow,
            "SELECT id, workload_id, node_id, lease_epoch, interval_start, interval_end, kind,
                    cause, supersedes, cpu_usec, cpu_throttled_usec, io_read_bytes,
                    io_write_bytes, net_rx_bytes, net_tx_bytes, memory_kib_seconds,
                    memory_peak_bytes, disk_bytes, losses, recorded_at
               FROM metering_record
              WHERE workload_id = $1 AND interval_start >= $2 AND interval_start < $3
              ORDER BY interval_start, recorded_at, id",
            workload,
            from_time(from),
            from_time(to)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(RecordRow::into_domain).collect()
    }

    async fn metering_usage(&self, q: &UsageQuery) -> StoreResult<Vec<Record>> {
        let (after_start, after_id) =
            q.after.map_or((None, None), |(t, id)| (Some(from_time(t)), Some(id)));
        let rows = sqlx::query_as!(
            RecordRow,
            "SELECT r.id, r.workload_id, r.node_id, r.lease_epoch, r.interval_start,
                    r.interval_end, r.kind, r.cause, r.supersedes, r.cpu_usec,
                    r.cpu_throttled_usec, r.io_read_bytes, r.io_write_bytes, r.net_rx_bytes,
                    r.net_tx_bytes, r.memory_kib_seconds, r.memory_peak_bytes, r.disk_bytes,
                    r.losses, r.recorded_at
               FROM metering_record r JOIN workload w ON w.id = r.workload_id
              WHERE ((NOT $10 OR r.kind <> 'adjustment')
                     AND r.interval_start >= $4 AND r.interval_start < $5
                  OR $10 AND r.kind = 'adjustment' AND r.recorded_at >= $4 AND r.recorded_at < $5)
                AND ($1::uuid IS NULL OR r.workload_id = $1)
                AND ($2::uuid IS NULL OR w.project_id = $2)
                AND ($3::uuid IS NULL OR w.organization_id IN (
                        WITH RECURSIVE tree AS (
                            SELECT id FROM organization WHERE id = $3
                            UNION ALL
                            SELECT o.id FROM organization o JOIN tree t ON o.parent_id = t.id)
                        SELECT id FROM tree))
                AND (NOT $6 OR NOT EXISTS (
                        SELECT 1 FROM metering_record s WHERE r.id = ANY (s.supersedes)
                           AND (NOT $10 OR s.kind <> 'adjustment')))
                AND ($7::timestamptz IS NULL OR (r.interval_start, r.id) > ($7, $8::uuid))
              ORDER BY r.interval_start, r.id LIMIT $9",
            q.workload,
            q.project,
            q.organization,
            from_time(q.from),
            from_time(q.to),
            q.current,
            after_start,
            after_id,
            i64::from(q.limit),
            q.by_period
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(RecordRow::into_domain).collect()
    }

    async fn metering_closed_days(
        &self,
        from: SystemTime,
        to: SystemTime,
    ) -> StoreResult<Vec<(SystemTime, SystemTime)>> {
        let rows = sqlx::query!(
            r#"SELECT (day::timestamp AT TIME ZONE 'UTC') AS "start!", closed_at
                 FROM metering_period
                WHERE day >= ($1::timestamptz AT TIME ZONE 'UTC')::date
                  AND day < ($2::timestamptz AT TIME ZONE 'UTC')::date
                ORDER BY day"#,
            from_time(from),
            from_time(to)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(|r| (to_time(r.start), to_time(r.closed_at))).collect())
    }
}
