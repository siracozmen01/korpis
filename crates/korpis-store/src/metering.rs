// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metering's readings and records (§2 of docs/15-observability.md).

use crate::errors::from_sqlx;
use crate::stored::{count, from_json, from_time, to_count, to_json, to_time};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::ports::{MeteringRead, MeteringWrite, StoreError, StoreResult};
use korpis_core::metering::{Counters, Loss, Reading, Record, RecordKind};
use std::time::SystemTime;
use uuid::Uuid;

/// A loss as it is stored among a record's.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct LossDoc {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    cause: String,
}

/// Losses, to store.
fn losses_doc(losses: &[Loss]) -> StoreResult<serde_json::Value> {
    let docs: Vec<LossDoc> = losses
        .iter()
        .map(|l| LossDoc { from: from_time(l.from), to: from_time(l.to), cause: l.cause.clone() })
        .collect();
    to_json("metering losses", &docs)
}

/// Losses, as stored.
fn losses(doc: serde_json::Value) -> StoreResult<Vec<Loss>> {
    let docs: Vec<LossDoc> = from_json("metering losses", doc)?;
    Ok(docs
        .into_iter()
        .map(|d| Loss { from: to_time(d.from), to: to_time(d.to), cause: d.cause })
        .collect())
}

fn stored(what: &str, n: Option<u64>) -> StoreResult<Option<i64>> {
    n.map(|n| to_count(what, n)).transpose()
}

fn read(what: &str, n: Option<i64>) -> StoreResult<Option<u64>> {
    n.map(|n| count(what, n)).transpose()
}

/// A counter row's columns, to store.
fn columns(c: &Counters) -> StoreResult<[Option<i64>; 7]> {
    Ok([
        stored("cpu_usec", c.cpu_usec)?,
        stored("cpu_throttled_usec", c.cpu_throttled_usec)?,
        stored("io_read_bytes", c.io_read_bytes)?,
        stored("io_write_bytes", c.io_write_bytes)?,
        stored("net_rx_bytes", c.net_rx_bytes)?,
        stored("net_tx_bytes", c.net_tx_bytes)?,
        stored("memory_kib_seconds", c.memory_kib_seconds)?,
    ])
}

/// Counters, as their columns hold them.
fn counters(c: [Option<i64>; 7]) -> StoreResult<Counters> {
    let [cpu, throttled, read_bytes, written, rx, tx, memory] = c;
    Ok(Counters {
        cpu_usec: read("cpu_usec", cpu)?,
        cpu_throttled_usec: read("cpu_throttled_usec", throttled)?,
        io_read_bytes: read("io_read_bytes", read_bytes)?,
        io_write_bytes: read("io_write_bytes", written)?,
        net_rx_bytes: read("net_rx_bytes", rx)?,
        net_tx_bytes: read("net_tx_bytes", tx)?,
        memory_kib_seconds: read("memory_kib_seconds", memory)?,
    })
}

impl MeteringWrite for PgTx {
    async fn metering_reading(
        &mut self,
        workload: Uuid,
        node: Uuid,
    ) -> StoreResult<Option<Reading>> {
        let row = sqlx::query!(
            "SELECT interval_end, cpu_usec, cpu_throttled_usec, io_read_bytes, io_write_bytes,
                    net_rx_bytes, net_tx_bytes, memory_kib_seconds
               FROM metering_reading WHERE workload_id = $1 AND node_id = $2 FOR UPDATE",
            workload,
            node
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            let c = [
                r.cpu_usec,
                r.cpu_throttled_usec,
                r.io_read_bytes,
                r.io_write_bytes,
                r.net_rx_bytes,
                r.net_tx_bytes,
                r.memory_kib_seconds,
            ];
            Ok(Reading { end: to_time(r.interval_end), counters: counters(c)? })
        })
        .transpose()
    }

    async fn keep_metering_reading(
        &mut self,
        workload: Uuid,
        node: Uuid,
        reading: &Reading,
    ) -> StoreResult<()> {
        let [cpu, throttled, read_bytes, written, rx, tx, memory] = columns(&reading.counters)?;
        sqlx::query!(
            "INSERT INTO metering_reading (workload_id, node_id, interval_end, cpu_usec,
                    cpu_throttled_usec, io_read_bytes, io_write_bytes, net_rx_bytes, net_tx_bytes,
                    memory_kib_seconds)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (workload_id, node_id) DO UPDATE SET interval_end = $3, cpu_usec = $4,
                    cpu_throttled_usec = $5, io_read_bytes = $6, io_write_bytes = $7,
                    net_rx_bytes = $8, net_tx_bytes = $9, memory_kib_seconds = $10",
            workload,
            node,
            from_time(reading.end),
            cpu,
            throttled,
            read_bytes,
            written,
            rx,
            tx,
            memory
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn append_metering_record(&mut self, r: &Record, at: SystemTime) -> StoreResult<()> {
        let [cpu, throttled, read_bytes, written, rx, tx, memory] = columns(&r.used)?;
        sqlx::query!(
            "INSERT INTO metering_record (id, recorded_at, workload_id, node_id, lease_epoch,
                    interval_start, interval_end, kind, cause, supersedes, cpu_usec,
                    cpu_throttled_usec, io_read_bytes, io_write_bytes, net_rx_bytes, net_tx_bytes,
                    memory_kib_seconds, memory_peak_bytes, disk_bytes, losses)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                     $18, $19, $20)",
            r.id,
            from_time(at),
            r.workload,
            r.node,
            to_count("lease epoch", r.lease_epoch)?,
            from_time(r.from),
            from_time(r.to),
            r.kind.as_str(),
            r.cause,
            r.supersedes,
            cpu,
            throttled,
            read_bytes,
            written,
            rx,
            tx,
            memory,
            stored("memory_peak_bytes", r.memory_peak_bytes)?,
            stored("disk_bytes", r.disk_bytes)?,
            losses_doc(&r.losses)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}

impl MeteringRead for PgStore {
    async fn metering_records(
        &self,
        workload: Uuid,
        from: SystemTime,
        to: SystemTime,
    ) -> StoreResult<Vec<Record>> {
        let rows = sqlx::query!(
            "SELECT id, workload_id, node_id, lease_epoch, interval_start, interval_end, kind,
                    cause, supersedes, cpu_usec, cpu_throttled_usec, io_read_bytes,
                    io_write_bytes, net_rx_bytes, net_tx_bytes, memory_kib_seconds,
                    memory_peak_bytes, disk_bytes, losses
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
        rows.into_iter()
            .map(|r| {
                let kind = RecordKind::parse(&r.kind)
                    .ok_or_else(|| StoreError::Corrupt(format!("metering kind {}", r.kind)))?;
                let used = [
                    r.cpu_usec,
                    r.cpu_throttled_usec,
                    r.io_read_bytes,
                    r.io_write_bytes,
                    r.net_rx_bytes,
                    r.net_tx_bytes,
                    r.memory_kib_seconds,
                ];
                Ok(Record {
                    id: r.id,
                    workload: r.workload_id,
                    node: r.node_id,
                    lease_epoch: count("lease epoch", r.lease_epoch)?,
                    from: to_time(r.interval_start),
                    to: to_time(r.interval_end),
                    kind,
                    cause: r.cause,
                    supersedes: r.supersedes,
                    used: counters(used)?,
                    memory_peak_bytes: read("memory_peak_bytes", r.memory_peak_bytes)?,
                    disk_bytes: read("disk_bytes", r.disk_bytes)?,
                    losses: losses(r.losses)?,
                })
            })
            .collect()
    }
}
