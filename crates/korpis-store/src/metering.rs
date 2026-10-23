// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metering's readings and records (§2 of docs/15-observability.md).

use crate::errors::from_sqlx;
use crate::stored::{count, from_json, from_time, to_count, to_json, to_time};

mod read;
use crate::PgTx;
use chrono::{DateTime, Utc};
use korpis_app::ports::{MeteringWrite, StoreResult};
use korpis_core::metering::{Counters, EndpointUse, Endpoints, Loss, Reading, Record};
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

/// An endpoint's counters as they are stored among a record's or a reading's.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct EndpointDoc {
    received_bytes: u64,
    received_packets: u64,
    sent_bytes: u64,
    sent_packets: u64,
}

/// Endpoints, to store.
fn endpoints_doc(endpoints: &Endpoints) -> StoreResult<serde_json::Value> {
    let docs: std::collections::BTreeMap<&String, EndpointDoc> = endpoints
        .iter()
        .map(|(name, e)| {
            let doc = EndpointDoc {
                received_bytes: e.received_bytes,
                received_packets: e.received_packets,
                sent_bytes: e.sent_bytes,
                sent_packets: e.sent_packets,
            };
            (name, doc)
        })
        .collect();
    to_json("metering endpoints", &docs)
}

/// Endpoints, as stored.
fn endpoints(doc: serde_json::Value) -> StoreResult<Endpoints> {
    let docs: std::collections::BTreeMap<String, EndpointDoc> =
        from_json("metering endpoints", doc)?;
    Ok(docs
        .into_iter()
        .map(|(name, d)| {
            let used = EndpointUse {
                received_bytes: d.received_bytes,
                received_packets: d.received_packets,
                sent_bytes: d.sent_bytes,
                sent_packets: d.sent_packets,
            };
            (name, used)
        })
        .collect())
}

fn stored(what: &str, n: Option<u64>) -> StoreResult<Option<i64>> {
    n.map(|n| to_count(what, n)).transpose()
}

fn read(what: &str, n: Option<i64>) -> StoreResult<Option<u64>> {
    n.map(|n| count(what, n)).transpose()
}

/// A counter row's columns, to store.
fn columns(c: &Counters) -> StoreResult<[Option<i64>; 8]> {
    Ok([
        stored("cpu_usec", c.cpu_usec)?,
        stored("cpu_throttled_usec", c.cpu_throttled_usec)?,
        stored("io_read_bytes", c.io_read_bytes)?,
        stored("io_write_bytes", c.io_write_bytes)?,
        stored("net_rx_bytes", c.net_rx_bytes)?,
        stored("net_tx_bytes", c.net_tx_bytes)?,
        stored("memory_kib_seconds", c.memory_kib_seconds)?,
        stored("egress_bytes", c.egress_bytes)?,
    ])
}

/// Counters, as their columns and their endpoints' document hold them.
fn counters(c: [Option<i64>; 8], doc: serde_json::Value) -> StoreResult<Counters> {
    let [cpu, throttled, read_bytes, written, rx, tx, memory, egress] = c;
    Ok(Counters {
        cpu_usec: read("cpu_usec", cpu)?,
        cpu_throttled_usec: read("cpu_throttled_usec", throttled)?,
        io_read_bytes: read("io_read_bytes", read_bytes)?,
        io_write_bytes: read("io_write_bytes", written)?,
        net_rx_bytes: read("net_rx_bytes", rx)?,
        net_tx_bytes: read("net_tx_bytes", tx)?,
        memory_kib_seconds: read("memory_kib_seconds", memory)?,
        egress_bytes: read("egress_bytes", egress)?,
        endpoints: endpoints(doc)?,
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
                    net_rx_bytes, net_tx_bytes, memory_kib_seconds, egress_bytes, endpoints
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
                r.egress_bytes,
            ];
            Ok(Reading { end: to_time(r.interval_end), counters: counters(c, r.endpoints)? })
        })
        .transpose()
    }

    async fn keep_metering_reading(
        &mut self,
        workload: Uuid,
        node: Uuid,
        reading: &Reading,
    ) -> StoreResult<()> {
        let [cpu, throttled, read_bytes, written, rx, tx, memory, egress] =
            columns(&reading.counters)?;
        sqlx::query!(
            "INSERT INTO metering_reading (workload_id, node_id, interval_end, cpu_usec,
                    cpu_throttled_usec, io_read_bytes, io_write_bytes, net_rx_bytes, net_tx_bytes,
                    memory_kib_seconds, egress_bytes, endpoints)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             ON CONFLICT (workload_id, node_id) DO UPDATE SET interval_end = $3, cpu_usec = $4,
                    cpu_throttled_usec = $5, io_read_bytes = $6, io_write_bytes = $7,
                    net_rx_bytes = $8, net_tx_bytes = $9, memory_kib_seconds = $10,
                    egress_bytes = $11, endpoints = $12",
            workload,
            node,
            from_time(reading.end),
            cpu,
            throttled,
            read_bytes,
            written,
            rx,
            tx,
            memory,
            egress,
            endpoints_doc(&reading.counters.endpoints)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn metering_gaps_within(
        &mut self,
        workload: Uuid,
        node: Uuid,
        (from, to): (SystemTime, SystemTime),
    ) -> StoreResult<Vec<Uuid>> {
        let rows = sqlx::query_scalar!(
            "SELECT g.id FROM metering_record g
              WHERE g.kind = 'gap' AND g.workload_id = $1 AND g.node_id = $2
                AND g.interval_start < $4 AND g.interval_end > $3
                AND NOT EXISTS (SELECT 1 FROM metering_record s WHERE g.id = ANY (s.supersedes))
              ORDER BY g.interval_start",
            workload,
            node,
            from_time(from),
            from_time(to)
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(rows)
    }

    async fn metering_day_closed(&mut self, day: SystemTime) -> StoreResult<bool> {
        let closed = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM metering_period
                               WHERE day = ($1::timestamptz AT TIME ZONE 'UTC')::date)
                      AS "closed!""#,
            from_time(day)
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(closed)
    }

    async fn mark_metering_gapped(
        &mut self,
        workload: Uuid,
        node: Uuid,
        (from, through): (SystemTime, SystemTime),
    ) -> StoreResult<()> {
        // A workload never sampled on its node has no reading yet: one is made, measuring
        // nothing, from where its gaps began, so its first sample is measured from there.
        sqlx::query!(
            "INSERT INTO metering_reading (workload_id, node_id, interval_end, gapped_through)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (workload_id, node_id) DO UPDATE SET gapped_through = $4",
            workload,
            node,
            from_time(from),
            from_time(through)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn close_metering_days(
        &mut self,
        before: SystemTime,
        now: SystemTime,
    ) -> StoreResult<u64> {
        let closed = sqlx::query!(
            "INSERT INTO metering_period (day, closed_at)
             SELECT d::date, $2 FROM generate_series(
                 coalesce((SELECT max(day) + 1 FROM metering_period),
                          (SELECT min((interval_start AT TIME ZONE 'UTC')::date)
                             FROM metering_record),
                          ($1::timestamptz AT TIME ZONE 'UTC')::date),
                 ($1::timestamptz AT TIME ZONE 'UTC')::date - 1,
                 interval '1 day') AS d
             ON CONFLICT (day) DO NOTHING",
            from_time(before),
            from_time(now)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(closed.rows_affected())
    }

    async fn append_metering_record(&mut self, r: &Record, at: SystemTime) -> StoreResult<()> {
        let [cpu, throttled, read_bytes, written, rx, tx, memory, egress] = columns(&r.used)?;
        sqlx::query!(
            "INSERT INTO metering_record (id, recorded_at, workload_id, node_id, lease_epoch,
                    interval_start, interval_end, kind, cause, supersedes, cpu_usec,
                    cpu_throttled_usec, io_read_bytes, io_write_bytes, net_rx_bytes, net_tx_bytes,
                    memory_kib_seconds, memory_peak_bytes, disk_bytes, losses, egress_bytes,
                    endpoints, disk_allocated)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                     $18, $19, $20, $21, $22, $23)",
            r.id,
            from_time(at),
            r.workload,
            r.node,
            to_count("lease epoch", r.lease_epoch)?,
            from_time(r.from),
            from_time(r.to),
            r.kind.as_str(),
            r.cause,
            &r.supersedes,
            cpu,
            throttled,
            read_bytes,
            written,
            rx,
            tx,
            memory,
            stored("memory_peak_bytes", r.memory_peak_bytes)?,
            stored("disk_bytes", r.disk_bytes)?,
            losses_doc(&r.losses)?,
            egress,
            endpoints_doc(&r.used.endpoints)?,
            r.disk_allocated
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
