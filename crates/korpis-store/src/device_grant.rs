// SPDX-License-Identifier: AGPL-3.0-or-later

//! Device authorizations (migration 0071), kept by the digest of their device code and by their
//! user code.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{DeviceGrantRead, DeviceGrantWrite, Polled, StoreError, StoreResult};
use korpis_core::account::device_grant::{Decision, DeviceAuthorization};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

struct Row {
    id: Uuid,
    user_code: String,
    created_at: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
    interval_secs: i32,
    user_agent: String,
    source: String,
    decided_by: Option<Uuid>,
    decided_at: Option<chrono::DateTime<chrono::Utc>>,
    approved: Option<bool>,
}

impl TryFrom<Row> for DeviceAuthorization {
    type Error = StoreError;

    fn try_from(r: Row) -> StoreResult<Self> {
        let interval = u64::try_from(r.interval_secs)
            .map_err(|_| StoreError::Corrupt(format!("device authorization {}: interval", r.id)))?;
        let decision = match (r.decided_by, r.decided_at, r.approved) {
            (Some(account), Some(at), Some(approved)) => {
                Some(Decision { account, at: to_time(at), approved })
            }
            _ => None,
        };
        Ok(Self {
            id: r.id,
            user_code: r.user_code,
            created_at: to_time(r.created_at),
            expires_at: to_time(r.expires_at),
            interval: Duration::from_secs(interval),
            user_agent: r.user_agent,
            source: r.source,
            decision,
        })
    }
}

impl DeviceGrantRead for PgStore {
    async fn device_authorization(&self, code: &str) -> StoreResult<Option<DeviceAuthorization>> {
        let row = sqlx::query_as!(
            Row,
            "SELECT id, user_code, created_at, expires_at, interval_secs, user_agent, source,
                    decided_by, decided_at, approved
               FROM device_authorization WHERE user_code = $1",
            code
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(DeviceAuthorization::try_from).transpose()
    }

    async fn forget_expired_device_authorizations(
        &self,
        now: SystemTime,
        limit: u32,
    ) -> StoreResult<u64> {
        let done = sqlx::query!(
            "DELETE FROM device_authorization
              WHERE id IN (SELECT id FROM device_authorization WHERE expires_at <= $1
                            LIMIT $2 FOR UPDATE SKIP LOCKED)",
            from_time(now),
            i64::from(limit)
        )
        .execute(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }
}

impl DeviceGrantWrite for PgTx {
    async fn insert_device_authorization(
        &mut self,
        a: &DeviceAuthorization,
        digest: &[u8; 32],
    ) -> StoreResult<bool> {
        let interval = i32::try_from(a.interval.as_secs()).unwrap_or(i32::MAX);
        let done = sqlx::query!(
            "INSERT INTO device_authorization (id, device_hash, user_code, created_at, expires_at,
                                               interval_secs, user_agent, source)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (user_code) DO NOTHING",
            a.id,
            digest.as_slice(),
            a.user_code,
            from_time(a.created_at),
            from_time(a.expires_at),
            interval,
            a.user_agent,
            a.source
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn poll_device(&mut self, digest: &[u8; 32], at: SystemTime) -> StoreResult<Polled> {
        let found = sqlx::query!(
            "SELECT id, user_code, created_at, expires_at, interval_secs, user_agent, source,
                    decided_by, decided_at, approved, last_polled_at
               FROM device_authorization WHERE device_hash = $1 FOR UPDATE",
            digest.as_slice()
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let Some(r) = found else { return Ok(Polled::Unknown) };
        sqlx::query!(
            "UPDATE device_authorization SET last_polled_at = $2 WHERE id = $1",
            r.id,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let polled_before = r.last_polled_at.map(to_time);
        let row = Row {
            id: r.id,
            user_code: r.user_code,
            created_at: r.created_at,
            expires_at: r.expires_at,
            interval_secs: r.interval_secs,
            user_agent: r.user_agent,
            source: r.source,
            decided_by: r.decided_by,
            decided_at: r.decided_at,
            approved: r.approved,
        };
        Ok(Polled::Found { authorization: DeviceAuthorization::try_from(row)?, polled_before })
    }

    async fn slow_device(&mut self, id: Uuid, seconds: u32) -> StoreResult<()> {
        let seconds = i32::try_from(seconds).unwrap_or(i32::MAX);
        sqlx::query!(
            "UPDATE device_authorization SET interval_secs = $2 WHERE id = $1",
            id,
            seconds
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn decide_device(
        &mut self,
        code: &str,
        account: Uuid,
        approved: bool,
        at: SystemTime,
    ) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE device_authorization SET decided_by = $2, decided_at = $3, approved = $4
              WHERE user_code = $1 AND decided_by IS NULL AND expires_at > $3",
            code,
            account,
            from_time(at),
            approved
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn delete_device_authorization(&mut self, id: Uuid) -> StoreResult<()> {
        sqlx::query!("DELETE FROM device_authorization WHERE id = $1", id)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }
}
