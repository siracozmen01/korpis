// SPDX-License-Identifier: AGPL-3.0-or-later

//! Streams' segment index (§4 of docs/14-streams.md), kept from what nodes journal.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{Expired, Reported, StoreError, StoreResult, StreamRead, StreamWrite};
use korpis_core::workload::stream::{Segment, SegmentEvent, Stream, StreamKey, StreamKind};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

fn offset(n: u64) -> StoreResult<i64> {
    i64::try_from(n).map_err(|_| StoreError::Corrupt(format!("offset {n} out of range")))
}

fn unsigned(n: i64) -> StoreResult<u64> {
    u64::try_from(n).map_err(|_| StoreError::Corrupt(format!("negative {n}")))
}

impl StreamRead for PgStore {
    async fn stream(&self, key: StreamKey) -> StoreResult<Option<Stream>> {
        let Some(id) = sqlx::query_scalar!(
            "SELECT id FROM stream
              WHERE workload_id = $1 AND kind = $2 AND run_id IS NOT DISTINCT FROM $3",
            key.workload,
            key.kind.as_str(),
            key.run
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?
        else {
            return Ok(None);
        };
        let rows = sqlx::query!(
            "SELECT first_offset, end_offset, first_at, last_at, bytes, digest, node_id,
                    repository_id
               FROM stream_segment WHERE stream_id = $1 ORDER BY first_offset",
            id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        let segments = rows
            .into_iter()
            .map(|r| {
                Ok(Segment {
                    first_offset: unsigned(r.first_offset)?,
                    end_offset: unsigned(r.end_offset)?,
                    first_at: to_time(r.first_at),
                    last_at: to_time(r.last_at),
                    bytes: unsigned(r.bytes)?,
                    digest: r.digest,
                    node: r.node_id,
                    repository: r.repository_id,
                })
            })
            .collect::<StoreResult<_>>()?;
        Ok(Some(Stream { id, workload: key.workload, run: key.run, kind: key.kind, segments }))
    }

    async fn expired_offloaded(
        &self,
        now: SystemTime,
        floor: Duration,
        limit: u32,
    ) -> StoreResult<Vec<Expired>> {
        let rows = sqlx::query!(
            r#"SELECT s.workload_id, s.kind, s.run_id, g.first_offset, g.digest,
                      g.repository_id AS "repository_id!"
                 FROM stream_segment g JOIN stream s ON s.id = g.stream_id
                WHERE g.repository_id IS NOT NULL
                  AND g.offloaded_at < $1::timestamptz - make_interval(secs => GREATEST(
                          CASE WHEN s.deleted_at IS NULL THEN s.offload_retention_s ELSE 0 END,
                          $2)::double precision)
                ORDER BY g.offloaded_at LIMIT $3"#,
            from_time(now),
            seconds(floor),
            i64::from(limit)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let kind = StreamKind::parse(&r.kind)
                    .ok_or_else(|| StoreError::Corrupt(format!("stream kind {}", r.kind)))?;
                Ok(Expired {
                    stream: StreamKey { workload: r.workload_id, kind, run: r.run_id },
                    first_offset: unsigned(r.first_offset)?,
                    digest: r.digest,
                    repository: r.repository_id,
                })
            })
            .collect()
    }
}

fn seconds(d: Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

impl StreamWrite for PgTx {
    async fn record_segment(
        &mut self,
        (key, offload_retention): (StreamKey, Duration),
        r: Reported<'_>,
    ) -> StoreResult<()> {
        let stream = sqlx::query_scalar!(
            "INSERT INTO stream (id, workload_id, kind, run_id, created_at, offload_retention_s)
             VALUES ($1, $2, $3, $4, now(), $5)
             ON CONFLICT (workload_id, kind, run_id) DO UPDATE
                SET offload_retention_s = EXCLUDED.offload_retention_s
             RETURNING id",
            Uuid::now_v7(),
            key.workload,
            key.kind.as_str(),
            key.run,
            seconds(offload_retention).max(1)
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let s = r.segment;
        let held = match r.event {
            SegmentEvent::Pruned => None,
            SegmentEvent::Sealed | SegmentEvent::Offloaded => Some(r.node),
        };
        let offloaded_at = s.repository.map(|_| from_time(r.at));
        sqlx::query!(
            "INSERT INTO stream_segment (stream_id, first_offset, end_offset, first_at, last_at,
                                         bytes, digest, node_id, repository_id, offloaded_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (stream_id, first_offset) DO UPDATE
                SET node_id = CASE WHEN stream_segment.node_id IS DISTINCT FROM $11 THEN
                                       stream_segment.node_id ELSE EXCLUDED.node_id END,
                    repository_id = COALESCE(stream_segment.repository_id,
                                             EXCLUDED.repository_id),
                    offloaded_at = CASE WHEN stream_segment.repository_id IS NULL
                                        THEN EXCLUDED.offloaded_at
                                        ELSE stream_segment.offloaded_at END",
            stream,
            offset(s.first_offset)?,
            offset(s.end_offset)?,
            from_time(s.first_at),
            from_time(s.last_at),
            offset(s.bytes)?,
            s.digest,
            held,
            s.repository,
            offloaded_at,
            r.node
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn end_stream(&mut self, key: StreamKey, at: SystemTime) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE stream SET deleted_at = $3
              WHERE workload_id = $1 AND kind = $2 AND run_id IS NOT DISTINCT FROM $4
                AND deleted_at IS NULL",
            key.workload,
            key.kind.as_str(),
            from_time(at),
            key.run
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn forget_offloaded(&mut self, e: &Expired) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE stream_segment g SET repository_id = NULL, offloaded_at = NULL
               FROM stream s
              WHERE s.id = g.stream_id AND s.workload_id = $1 AND s.kind = $2
                AND s.run_id IS NOT DISTINCT FROM $5
                AND g.first_offset = $3 AND g.repository_id = $4",
            e.stream.workload,
            e.stream.kind.as_str(),
            offset(e.first_offset)?,
            e.repository,
            e.stream.run
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
