// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rate limit buckets (migration 0065), charged by one statement.

use crate::PgStore;
use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use korpis_app::ports::{RateBuckets, StoreError, StoreResult};
use korpis_core::rate::{Bucket, Standing};
use std::time::SystemTime;
use uuid::Uuid;

impl RateBuckets for PgStore {
    async fn charge(&self, buckets: Vec<Bucket>, now: SystemTime) -> StoreResult<Vec<Standing>> {
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
        .fetch_all(&self.pool)
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
