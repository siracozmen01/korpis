// SPDX-License-Identifier: AGPL-3.0-or-later

//! Streams' segment index (§4 of docs/14-streams.md), kept from what nodes journal.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{StoreError, StoreResult, StreamRead, StreamWrite};
use korpis_core::workload::stream::{Segment, SegmentEvent, Stream, StreamKind};
use uuid::Uuid;

fn offset(n: u64) -> StoreResult<i64> {
    i64::try_from(n).map_err(|_| StoreError::Corrupt(format!("offset {n} out of range")))
}

fn unsigned(n: i64) -> StoreResult<u64> {
    u64::try_from(n).map_err(|_| StoreError::Corrupt(format!("negative {n}")))
}

impl StreamRead for PgStore {
    async fn stream(&self, workload: Uuid, kind: StreamKind) -> StoreResult<Option<Stream>> {
        let Some(id) = sqlx::query_scalar!(
            "SELECT id FROM stream WHERE workload_id = $1 AND kind = $2",
            workload,
            kind.as_str()
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
        Ok(Some(Stream { id, workload, kind, segments }))
    }
}

impl StreamWrite for PgTx {
    async fn record_segment(
        &mut self,
        (workload, kind): (Uuid, StreamKind),
        event: SegmentEvent,
        s: &Segment,
        node: Uuid,
    ) -> StoreResult<()> {
        let stream = sqlx::query_scalar!(
            "INSERT INTO stream (id, workload_id, kind, created_at) VALUES ($1, $2, $3, now())
             ON CONFLICT (workload_id, kind) DO UPDATE SET kind = EXCLUDED.kind
             RETURNING id",
            Uuid::now_v7(),
            workload,
            kind.as_str()
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let held = match event {
            SegmentEvent::Pruned => None,
            SegmentEvent::Sealed | SegmentEvent::Offloaded => Some(node),
        };
        sqlx::query!(
            "INSERT INTO stream_segment (stream_id, first_offset, end_offset, first_at, last_at,
                                         bytes, digest, node_id, repository_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (stream_id, first_offset) DO UPDATE
                SET node_id = CASE WHEN stream_segment.node_id IS DISTINCT FROM $10 THEN
                                       stream_segment.node_id ELSE EXCLUDED.node_id END,
                    repository_id = COALESCE(stream_segment.repository_id,
                                             EXCLUDED.repository_id)",
            stream,
            offset(s.first_offset)?,
            offset(s.end_offset)?,
            from_time(s.first_at),
            from_time(s.last_at),
            offset(s.bytes)?,
            s.digest,
            held,
            s.repository,
            node
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
