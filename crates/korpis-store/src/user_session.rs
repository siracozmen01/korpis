// SPDX-License-Identifier: AGPL-3.0-or-later

//! User sessions (migration 0069), kept by the digest of their current refresh credential and
//! every digest they rotated away.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{Presented, StoreError, StoreResult, UserSessionRead, UserSessionWrite};
use korpis_core::account::session::{SessionKind, UserSession};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

struct SessionRow {
    id: Uuid,
    account_id: Uuid,
    kind: String,
    created_at: chrono::DateTime<chrono::Utc>,
    last_used_at: chrono::DateTime<chrono::Utc>,
    authenticated_at: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
    user_agent: String,
    source: String,
}

impl TryFrom<SessionRow> for UserSession {
    type Error = StoreError;

    fn try_from(r: SessionRow) -> StoreResult<Self> {
        Ok(Self {
            id: r.id,
            account: r.account_id,
            kind: SessionKind::parse(&r.kind)
                .ok_or_else(|| StoreError::Corrupt(format!("session {}: kind", r.id)))?,
            created_at: to_time(r.created_at),
            last_used_at: to_time(r.last_used_at),
            authenticated_at: to_time(r.authenticated_at),
            expires_at: to_time(r.expires_at),
            user_agent: r.user_agent,
            source: r.source,
        })
    }
}

impl UserSessionRead for PgStore {
    async fn user_sessions(&self, account: Uuid) -> StoreResult<Vec<UserSession>> {
        let rows = sqlx::query_as!(
            SessionRow,
            "SELECT id, account_id, kind, created_at, last_used_at, authenticated_at, expires_at,
                    user_agent, source
               FROM user_session WHERE account_id = $1 ORDER BY created_at DESC, id",
            account
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(UserSession::try_from).collect()
    }

    async fn user_session(&self, account: Uuid, id: Uuid) -> StoreResult<Option<UserSession>> {
        let row = sqlx::query_as!(
            SessionRow,
            "SELECT id, account_id, kind, created_at, last_used_at, authenticated_at, expires_at,
                    user_agent, source
               FROM user_session WHERE account_id = $1 AND id = $2",
            account,
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(UserSession::try_from).transpose()
    }

    async fn forget_lapsed_sessions(
        &self,
        now: SystemTime,
        idle: Duration,
        limit: u32,
    ) -> StoreResult<u64> {
        let idle_since = now.checked_sub(idle).unwrap_or(SystemTime::UNIX_EPOCH);
        let done = sqlx::query!(
            "DELETE FROM user_session
              WHERE id IN (SELECT id FROM user_session
                            WHERE expires_at <= $1 OR last_used_at <= $2
                            LIMIT $3 FOR UPDATE SKIP LOCKED)",
            from_time(now),
            from_time(idle_since),
            i64::from(limit)
        )
        .execute(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }
}

impl UserSessionWrite for PgTx {
    async fn insert_user_session(&mut self, s: &UserSession, digest: &[u8; 32]) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO user_session (id, account_id, kind, refresh_hash, created_at,
                                       last_used_at, authenticated_at, expires_at, user_agent,
                                       source)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            s.id,
            s.account,
            s.kind.as_str(),
            digest.as_slice(),
            from_time(s.created_at),
            from_time(s.last_used_at),
            from_time(s.authenticated_at),
            from_time(s.expires_at),
            s.user_agent,
            s.source
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn present_refresh(&mut self, digest: &[u8; 32]) -> StoreResult<Presented> {
        let current = sqlx::query!(
            r#"SELECT s.id, s.account_id, s.kind, s.created_at, s.last_used_at, s.authenticated_at,
                      s.expires_at, s.user_agent, s.source, a.disabled_at IS NULL AS "usable!"
                 FROM user_session s JOIN account a ON a.id = s.account_id
                WHERE s.refresh_hash = $1 FOR UPDATE OF s"#,
            digest.as_slice()
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        if let Some(r) = current {
            let row = SessionRow {
                id: r.id,
                account_id: r.account_id,
                kind: r.kind,
                created_at: r.created_at,
                last_used_at: r.last_used_at,
                authenticated_at: r.authenticated_at,
                expires_at: r.expires_at,
                user_agent: r.user_agent,
                source: r.source,
            };
            return Ok(Presented::Current {
                session: UserSession::try_from(row)?,
                usable: r.usable,
            });
        }
        let rotated = sqlx::query!(
            "SELECT r.session_id, s.account_id
               FROM user_session_rotated r JOIN user_session s ON s.id = r.session_id
              WHERE r.refresh_hash = $1",
            digest.as_slice()
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(rotated.map_or(Presented::Unknown, |r| Presented::Rotated {
            session: r.session_id,
            account: r.account_id,
        }))
    }

    async fn rotate_refresh(
        &mut self,
        id: Uuid,
        old: &[u8; 32],
        new: &[u8; 32],
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO user_session_rotated (refresh_hash, session_id) VALUES ($1, $2)",
            old.as_slice(),
            id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        sqlx::query!(
            "UPDATE user_session SET refresh_hash = $2, last_used_at = $3 WHERE id = $1",
            id,
            new.as_slice(),
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn reauthenticated(
        &mut self,
        account: Uuid,
        id: Uuid,
        at: SystemTime,
    ) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE user_session SET authenticated_at = $3 WHERE account_id = $1 AND id = $2",
            account,
            id,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn end_user_session(&mut self, account: Uuid, id: Uuid) -> StoreResult<bool> {
        let done =
            sqlx::query!("DELETE FROM user_session WHERE id = $1 AND account_id = $2", id, account)
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn end_every_session(&mut self) -> StoreResult<u64> {
        let done = sqlx::query!("DELETE FROM user_session")
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }

    async fn end_other_sessions(&mut self, account: Uuid, keep: Option<Uuid>) -> StoreResult<u64> {
        let done = sqlx::query!(
            "DELETE FROM user_session WHERE account_id = $1 AND id IS DISTINCT FROM $2",
            account,
            keep
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }
}
