// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rate limit buckets (migration 0065), charged by one statement.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{RateBuckets, RateWrite, StoreError, StoreResult};
use korpis_core::rate::{Bucket, Standing};
use std::time::SystemTime;
use uuid::Uuid;

/// Charges `buckets` at `now` through `e`, a pool or a transaction.
async fn charge<'e, E: sqlx::PgExecutor<'e>>(
    e: E,
    buckets: Vec<Bucket>,
    now: SystemTime,
) -> StoreResult<Vec<Standing>> {
    if buckets.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<String> = buckets.iter().map(|b| b.key.clone()).collect();
    let micros = |b: &Bucket| i64::try_from(b.rate.interval().as_micros()).unwrap_or(i64::MAX);
    let intervals: Vec<i64> = buckets.iter().map(micros).collect();
    let bursts: Vec<i32> =
        buckets.iter().map(|b| i32::try_from(b.rate.burst).unwrap_or(i32::MAX)).collect();
    let rows = sqlx::query!(
        r#"SELECT key AS "key!", admitted AS "admitted!", full_at AS "full_at!"
             FROM korpis_rate_charge($1, $2, $3, $4)"#,
        &keys,
        &intervals,
        &bursts,
        from_time(now)
    )
    .fetch_all(e)
    .await
    .map_err(from_sqlx)?;
    buckets
        .into_iter()
        .map(|bucket| {
            let row = rows.iter().find(|r| r.key == bucket.key).ok_or_else(|| {
                StoreError::Corrupt(format!("rate bucket {} not answered", bucket.key))
            })?;
            let full_in = to_time(row.full_at).duration_since(now).unwrap_or_default();
            Ok(Standing { bucket, admitted: row.admitted, full_in })
        })
        .collect()
}

impl RateWrite for PgTx {
    async fn charge_within(
        &mut self,
        buckets: Vec<Bucket>,
        now: SystemTime,
    ) -> StoreResult<Vec<Standing>> {
        charge(&mut *self.tx, buckets, now).await
    }
}

impl RateBuckets for PgStore {
    async fn charge(&self, buckets: Vec<Bucket>, now: SystemTime) -> StoreResult<Vec<Standing>> {
        charge(&self.pool, buckets, now).await
    }

    async fn peek(&self, buckets: Vec<Bucket>, now: SystemTime) -> StoreResult<Vec<Standing>> {
        let keys: Vec<String> = buckets.iter().map(|b| b.key.clone()).collect();
        let rows = sqlx::query!("SELECT key, full_at FROM rate_bucket WHERE key = ANY($1)", &keys)
            .fetch_all(&self.pool)
            .await
            .map_err(from_sqlx)?;
        Ok(buckets
            .into_iter()
            .map(|bucket| {
                let full_at = rows.iter().find(|r| r.key == bucket.key).map(|r| to_time(r.full_at));
                let full_in = full_at.and_then(|t| t.duration_since(now).ok()).unwrap_or_default();
                let admitted = bucket.rate.admits(full_in);
                Standing { bucket, admitted, full_in }
            })
            .collect())
    }

    async fn refund(&self, bucket: &Bucket, now: SystemTime) -> StoreResult<()> {
        let micros = i64::try_from(bucket.rate.interval().as_micros()).unwrap_or(i64::MAX);
        sqlx::query!(
            "UPDATE rate_bucket
                SET full_at = greatest(full_at - make_interval(secs => $2::bigint / 1000000.0), $3)
              WHERE key = $1",
            bucket.key,
            micros,
            from_time(now)
        )
        .execute(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn forget_full_buckets(&self, now: SystemTime, limit: u32) -> StoreResult<u64> {
        let done = sqlx::query!(
            "DELETE FROM rate_bucket
              WHERE key IN (SELECT key FROM rate_bucket WHERE full_at <= $1
                             LIMIT $2 FOR UPDATE SKIP LOCKED)",
            from_time(now),
            i64::from(limit)
        )
        .execute(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }

    async fn organizations_held(
        &self,
        subject_key: &str,
        now: SystemTime,
    ) -> StoreResult<Vec<Uuid>> {
        sqlx::query_file_scalar!("queries/organizations_held.sql", subject_key, from_time(now))
            .fetch_all(&self.pool)
            .await
            .map_err(from_sqlx)
    }
}
