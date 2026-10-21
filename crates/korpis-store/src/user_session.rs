// SPDX-License-Identifier: AGPL-3.0-or-later

//! User sessions (migrations 0069 and 0101), kept by the digest of their current refresh
//! credential and every digest they rotated away, held by an account or by a person's binding,
//! never both and never neither.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{Presented, StoreError, StoreResult, UserSessionRead, UserSessionWrite};
use korpis_core::account::session::{AtPortal, Holder, SessionKind, UserSession};
use korpis_core::principal::SetName;
use serde_json::Value;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

struct SessionRow {
    id: Uuid,
    account_id: Option<Uuid>,
    binding_id: Option<Uuid>,
    sets: Value,
    kind: String,
    created_at: chrono::DateTime<chrono::Utc>,
    last_used_at: chrono::DateTime<chrono::Utc>,
    authenticated_at: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
    user_agent: String,
    source: String,
    portal_id: Option<Uuid>,
    portal_organization: Option<Uuid>,
}

impl TryFrom<SessionRow> for UserSession {
    type Error = StoreError;

    fn try_from(r: SessionRow) -> StoreResult<Self> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("session {}: {what}", r.id));
        let holder = match (r.account_id, r.binding_id) {
            (Some(a), None) => Holder::Account(a),
            (None, Some(b)) => Holder::Person(b),
            _ => return Err(corrupt("holder")),
        };
        // A session's portal is never read without its organization, which confines it: one
        // missing would leave the session reaching everything.
        let portal = match (r.portal_id, r.portal_organization) {
            (Some(portal), Some(organization)) => Some(AtPortal { portal, organization }),
            (None, _) => None,
            (Some(_), None) => return Err(corrupt("portal")),
        };
        Ok(Self {
            id: r.id,
            holder,
            sets: sets_from(&r.sets).ok_or_else(|| corrupt("sets"))?,
            kind: SessionKind::parse(&r.kind)
                .ok_or_else(|| StoreError::Corrupt(format!("session {}: kind", r.id)))?,
            created_at: to_time(r.created_at),
            last_used_at: to_time(r.last_used_at),
            authenticated_at: to_time(r.authenticated_at),
            expires_at: to_time(r.expires_at),
            user_agent: r.user_agent,
            source: r.source,
            portal,
        })
    }
}

fn sets_from(v: &Value) -> Option<Vec<SetName>> {
    v.as_array()?
        .iter()
        .map(|s| {
            Some(SetName {
                realm: s.get("realm")?.as_str()?.to_owned(),
                member_of: s.get("member_of")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

fn sets_to(sets: &[SetName]) -> Value {
    sets.iter().map(|s| serde_json::json!({ "realm": s.realm, "member_of": s.member_of })).collect()
}

impl UserSessionRead for PgStore {
    async fn user_sessions(&self, holder: Holder) -> StoreResult<Vec<UserSession>> {
        let rows = sqlx::query_as!(
            SessionRow,
            "SELECT s.id, s.account_id, s.binding_id, s.sets, s.kind, s.created_at, s.last_used_at,
                    s.authenticated_at, s.expires_at, s.user_agent, s.source, s.portal_id,
                    p.organization_id AS \"portal_organization?\"
               FROM user_session s LEFT JOIN portal p ON p.id = s.portal_id
              WHERE (s.account_id = $1 OR s.binding_id = $2)
              ORDER BY s.created_at DESC, s.id",
            holder.account(),
            holder.binding()
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(UserSession::try_from).collect()
    }

    async fn user_session(&self, holder: Holder, id: Uuid) -> StoreResult<Option<UserSession>> {
        let row = sqlx::query_as!(
            SessionRow,
            "SELECT s.id, s.account_id, s.binding_id, s.sets, s.kind, s.created_at, s.last_used_at,
                    s.authenticated_at, s.expires_at, s.user_agent, s.source, s.portal_id,
                    p.organization_id AS \"portal_organization?\"
               FROM user_session s LEFT JOIN portal p ON p.id = s.portal_id
              WHERE (s.account_id = $1 OR s.binding_id = $2)
                AND s.id = $3",
            holder.account(),
            holder.binding(),
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
            "INSERT INTO user_session (id, account_id, binding_id, sets, kind, refresh_hash,
                                       created_at, last_used_at, authenticated_at, expires_at,
                                       user_agent, source, portal_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            s.id,
            s.holder.account(),
            s.holder.binding(),
            sets_to(&s.sets),
            s.kind.as_str(),
            digest.as_slice(),
            from_time(s.created_at),
            from_time(s.last_used_at),
            from_time(s.authenticated_at),
            from_time(s.expires_at),
            s.user_agent,
            s.source,
            s.portal.map(|p| p.portal)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn present_refresh(&mut self, digest: &[u8; 32]) -> StoreResult<Presented> {
        let current = sqlx::query!(
            r#"SELECT s.id, s.account_id, s.binding_id, s.sets, s.kind, s.created_at,
                      s.last_used_at, s.authenticated_at, s.expires_at, s.user_agent, s.source,
                      s.portal_id, p.organization_id AS "portal_organization?",
                      CASE WHEN s.account_id IS NOT NULL THEN a.disabled_at IS NULL
                           ELSE b.retired_at IS NULL END AS "usable!"
                 FROM user_session s
                 LEFT JOIN account a ON a.id = s.account_id
                 LEFT JOIN identity_binding b ON b.id = s.binding_id
                 LEFT JOIN portal p ON p.id = s.portal_id
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
                binding_id: r.binding_id,
                sets: r.sets,
                kind: r.kind,
                created_at: r.created_at,
                last_used_at: r.last_used_at,
                authenticated_at: r.authenticated_at,
                expires_at: r.expires_at,
                user_agent: r.user_agent,
                source: r.source,
                portal_id: r.portal_id,
                portal_organization: r.portal_organization,
            };
            return Ok(Presented::Current {
                session: UserSession::try_from(row)?,
                usable: r.usable,
            });
        }
        let rotated = sqlx::query!(
            "SELECT r.session_id, s.account_id, s.binding_id
               FROM user_session_rotated r JOIN user_session s ON s.id = r.session_id
              WHERE r.refresh_hash = $1",
            digest.as_slice()
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let Some(r) = rotated else { return Ok(Presented::Unknown) };
        let holder = match (r.account_id, r.binding_id) {
            (Some(a), _) => Holder::Account(a),
            (None, Some(b)) => Holder::Person(b),
            (None, None) => return Err(StoreError::Corrupt(format!("session {}", r.session_id))),
        };
        Ok(Presented::Rotated { session: r.session_id, holder })
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
        holder: Holder,
        id: Uuid,
        at: SystemTime,
    ) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE user_session SET authenticated_at = $4
              WHERE (account_id = $1 OR binding_id = $2)
                AND id = $3",
            holder.account(),
            holder.binding(),
            id,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn end_user_session(&mut self, holder: Holder, id: Uuid) -> StoreResult<bool> {
        let done = sqlx::query!(
            "DELETE FROM user_session
              WHERE id = $1 AND (account_id = $2 OR binding_id = $3)",
            id,
            holder.account(),
            holder.binding()
        )
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

    async fn end_other_sessions(&mut self, holder: Holder, keep: Option<Uuid>) -> StoreResult<u64> {
        let done = sqlx::query!(
            "DELETE FROM user_session
              WHERE (account_id = $1 OR binding_id = $2)
                AND id IS DISTINCT FROM $3",
            holder.account(),
            holder.binding(),
            keep
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }
}
