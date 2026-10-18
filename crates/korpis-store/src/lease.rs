// SPDX-License-Identifier: AGPL-3.0-or-later

//! Leases and the epoch sequence.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{count, from_time, to_count, to_time};
use korpis_app::ports::{EpochIssue, LeaseWrite, StoreError, StoreResult};
use korpis_core::workload::lease::Lease;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

fn term_ms(term: Duration) -> StoreResult<i64> {
    i64::try_from(term.as_millis()).map_err(|_| StoreError::Corrupt("lease term".into()))
}

impl LeaseWrite for PgTx {
    async fn lease_for_update(&mut self, workload: Uuid) -> StoreResult<Option<Lease>> {
        let row = sqlx::query!(
            "SELECT workload_id, node_id, epoch, term_ms, received_at, released_at
               FROM lease WHERE workload_id = $1 FOR UPDATE",
            workload
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            Ok(Lease {
                workload: r.workload_id,
                node: r.node_id,
                epoch: count("lease epoch", r.epoch)?,
                term: Duration::from_millis(count("lease term", r.term_ms)?),
                received_at: to_time(r.received_at),
                released_at: r.released_at.map(to_time),
            })
        })
        .transpose()
    }

    async fn next_epoch(&mut self, now: SystemTime) -> StoreResult<EpochIssue> {
        // The singleton row is created with the schema; the update locks it, so two issuers
        // cannot take the same epoch. The bucket refills at the rate and holds at most a second's
        // worth (§8 of docs/03-state.md).
        let issued = sqlx::query_scalar!(
            "UPDATE epoch_state SET
                 bucket_tokens = LEAST(issue_rate_per_sec::double precision, bucket_tokens
                     + issue_rate_per_sec * GREATEST(0, EXTRACT(EPOCH FROM $1 - bucket_at)))
                     - 1,
                 bucket_at = GREATEST(bucket_at, $1),
                 max_issued_epoch = GREATEST(max_issued_epoch, floor) + 1
              WHERE singleton
                AND (settling_until IS NULL OR settling_until <= $1)
                AND LEAST(issue_rate_per_sec::double precision, bucket_tokens
                     + issue_rate_per_sec * GREATEST(0, EXTRACT(EPOCH FROM $1 - bucket_at))) >= 1
              RETURNING max_issued_epoch",
            from_time(now)
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        if let Some(next) = issued {
            return Ok(EpochIssue::Issued(count("epoch", next)?));
        }
        let settling = self.settling(now).await?;
        Ok(if settling.is_some() { EpochIssue::Settling } else { EpochIssue::RateSpent })
    }

    async fn fence_epochs(&mut self, floor: u64, settling_until: SystemTime) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE epoch_state SET floor = GREATEST(floor, $1), settling_until = $2
              WHERE singleton",
            to_count("epoch floor", floor)?,
            from_time(settling_until)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn fence_epochs_pending(
        &mut self,
        floor: u64,
        settle: std::time::Duration,
    ) -> StoreResult<()> {
        let ms = i64::try_from(settle.as_millis().max(1))
            .map_err(|_| StoreError::Failed("the settling window is too long".into()))?;
        sqlx::query!(
            "UPDATE epoch_state SET floor = GREATEST(floor, $1), settle_pending_ms = $2
              WHERE singleton",
            to_count("epoch floor", floor)?,
            ms
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn open_pending_settle(&mut self, now: SystemTime) -> StoreResult<Option<SystemTime>> {
        let until = sqlx::query_scalar!(
            "UPDATE epoch_state
                SET settling_until = $1::timestamptz + settle_pending_ms * interval '1 millisecond',
                    settle_pending_ms = NULL
              WHERE singleton AND settle_pending_ms IS NOT NULL
             RETURNING settling_until",
            from_time(now)
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(until.flatten().map(to_time))
    }

    async fn raise_epoch_floor(&mut self, epoch: u64) -> StoreResult<bool> {
        let raised = sqlx::query!(
            "UPDATE epoch_state SET floor = $1 WHERE singleton AND floor < $1",
            to_count("epoch floor", epoch)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(raised.rows_affected() > 0)
    }

    async fn settling(&mut self, now: SystemTime) -> StoreResult<Option<SystemTime>> {
        let until = sqlx::query_scalar!(
            "SELECT settling_until FROM epoch_state WHERE singleton AND settling_until > $1",
            from_time(now)
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(until.flatten().map(to_time))
    }

    async fn put_lease(&mut self, l: &Lease) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO lease (workload_id, node_id, epoch, term_ms, received_at, grantor_expiry,
                                released_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (workload_id) DO UPDATE SET
                 node_id = EXCLUDED.node_id, epoch = EXCLUDED.epoch, term_ms = EXCLUDED.term_ms,
                 received_at = EXCLUDED.received_at, grantor_expiry = EXCLUDED.grantor_expiry,
                 released_at = EXCLUDED.released_at",
            l.workload,
            l.node,
            to_count("lease epoch", l.epoch)?,
            term_ms(l.term)?,
            from_time(l.received_at),
            from_time(l.grantor_expiry()),
            l.released_at.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn renew_lease(&mut self, workload: Uuid, received_at: SystemTime) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE lease SET received_at = $2::timestamptz,
                              grantor_expiry = $2::timestamptz + term_ms * interval '1 millisecond'
              WHERE workload_id = $1",
            workload,
            from_time(received_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn release_lease(
        &mut self,
        workload: Uuid,
        node: Uuid,
        epoch: u64,
        at: SystemTime,
    ) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE lease SET released_at = $4
              WHERE workload_id = $1 AND node_id = $2 AND epoch = $3 AND released_at IS NULL",
            workload,
            node,
            to_count("lease epoch", epoch)?,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
