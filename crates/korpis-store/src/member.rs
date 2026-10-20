// SPDX-License-Identifier: AGPL-3.0-or-later

//! The members of bound sets who delegated (migration 0100).

use crate::errors::from_sqlx;
use crate::stored::{from_time, principal, to_time};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::ports::{MemberRead, MemberWrite, StoreError, StoreResult};
use korpis_core::authz::binding::BindingState;
use korpis_core::authz::membership::MemberRecord;
use std::time::SystemTime;
use uuid::Uuid;

struct MemberRow {
    binding_id: Uuid,
    member: String,
    state: String,
    last_verified_at: Option<DateTime<Utc>>,
}

impl MemberRow {
    fn into_domain(self) -> StoreResult<MemberRecord> {
        let state = match self.state.as_str() {
            "verified" => BindingState::Verified,
            "unverified" => BindingState::Unverified,
            other => return Err(StoreError::Corrupt(format!("member state {other:?}"))),
        };
        Ok(MemberRecord {
            binding: self.binding_id,
            member: principal(&self.member)?,
            state,
            last_verified_at: self.last_verified_at.map(to_time),
        })
    }
}

impl MemberRead for PgStore {
    async fn member(&self, binding: Uuid, member: &str) -> StoreResult<Option<MemberRecord>> {
        let row = sqlx::query_as!(
            MemberRow,
            "SELECT binding_id, member, state, last_verified_at FROM identity_member
              WHERE binding_id = $1 AND member = $2",
            binding,
            member
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(MemberRow::into_domain).transpose()
    }

    async fn members_who_delegated(&self, binding: Uuid) -> StoreResult<Vec<MemberRecord>> {
        let rows = sqlx::query_as!(
            MemberRow,
            "SELECT m.binding_id, m.member, m.state, m.last_verified_at
               FROM identity_member m JOIN identity_binding b ON b.id = m.binding_id
              WHERE m.binding_id = $1
                AND EXISTS (SELECT 1 FROM grants g
                             WHERE g.revoked_at IS NULL AND g.via ->> 'set' = b.subject
                               AND g.via ->> 'member' = m.member)
              ORDER BY m.member",
            binding
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(MemberRow::into_domain).collect()
    }
}

impl MemberWrite for PgTx {
    async fn record_members(
        &mut self,
        binding: Uuid,
        (present, absent): (&[String], &[String]),
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE identity_member
                SET state = CASE WHEN member = ANY($2) THEN 'verified' ELSE 'unverified' END,
                    last_verified_at = CASE WHEN member = ANY($2) THEN $4 ELSE last_verified_at END
              WHERE binding_id = $1 AND (member = ANY($2) OR member = ANY($3))",
            binding,
            present,
            absent,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
