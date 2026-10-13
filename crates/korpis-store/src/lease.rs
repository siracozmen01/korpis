// SPDX-License-Identifier: AGPL-3.0-or-later

//! Leases and the epoch sequence.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{count, from_time, to_count, to_time};
use korpis_app::ports::{LeaseWrite, StoreError, StoreResult};
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

    async fn next_epoch(&mut self) -> StoreResult<u64> {
        // The singleton row is created with the schema; the update locks it, so two issuers
        // cannot take the same epoch.
        let next = sqlx::query_scalar!(
            "UPDATE epoch_state SET max_issued_epoch = GREATEST(max_issued_epoch, floor) + 1
              WHERE singleton RETURNING max_issued_epoch"
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        count("epoch", next)
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
