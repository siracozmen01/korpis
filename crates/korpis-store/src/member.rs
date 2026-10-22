// SPDX-License-Identifier: AGPL-3.0-or-later

//! The members of bound sets who delegated (migration 0100).

use crate::errors::from_sqlx;
use crate::stored::{from_time, principal, to_time};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::ports::{MemberRead, MemberWrite, StoreError, StoreResult};
use korpis_core::authz::binding::{BindingState, ProviderRef};
use korpis_core::authz::membership::MemberRecord;
use std::time::SystemTime;
use uuid::Uuid;

struct MemberRow {
    binding_id: Uuid,
    member: String,
    state: String,
    last_verified_at: Option<DateTime<Utc>>,
    answers: i64,
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
            answers: u64::try_from(self.answers)
                .map_err(|_| StoreError::Corrupt("member answers".into()))?,
        })
    }
}

/// Each member asked about and the count of answers their record had then, as two columns.
fn columns(asked: &[(String, u64)]) -> StoreResult<(Vec<String>, Vec<i64>)> {
    let counts = asked.iter().map(|(_, n)| i64::try_from(*n)).collect::<Result<_, _>>();
    let counts = counts.map_err(|_| StoreError::Corrupt("member answers".into()))?;
    Ok((asked.iter().map(|(m, _)| m.clone()).collect(), counts))
}

impl MemberRead for PgStore {
    async fn member(&self, binding: Uuid, member: &str) -> StoreResult<Option<MemberRecord>> {
        let row = sqlx::query_as!(
            MemberRow,
            "SELECT binding_id, member, state, last_verified_at, answers FROM identity_member
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
            "SELECT m.binding_id, m.member, m.state, m.last_verified_at, m.answers
               FROM identity_member m JOIN identity_binding b ON b.id = m.binding_id
              WHERE m.binding_id = $1
                AND (EXISTS (SELECT 1 FROM grants g
                              WHERE g.revoked_at IS NULL AND g.via ->> 'set' = b.subject
                                AND g.via ->> 'member' = m.member)
                     OR EXISTS (SELECT 1 FROM user_session s
                                  JOIN identity_binding p ON p.id = s.binding_id
                                 WHERE p.subject = m.member AND s.expires_at > now()))
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
        (present, absent): (&[(String, u64)], &[(String, u64)]),
        at: SystemTime,
    ) -> StoreResult<u64> {
        let (present, present_at) = columns(present)?;
        let (absent, absent_at) = columns(absent)?;
        let stale = sqlx::query_scalar!(
            r#"WITH asked AS (
                    SELECT p.member, p.answers, true AS present
                      FROM unnest($2::text[], $3::bigint[]) AS p(member, answers)
                    UNION ALL
                    SELECT a.member, a.answers, false
                      FROM unnest($4::text[], $5::bigint[]) AS a(member, answers)),
                written AS (
                    UPDATE identity_member m
                       SET state = CASE WHEN a.present THEN 'verified' ELSE 'unverified' END,
                           last_verified_at =
                               CASE WHEN a.present THEN $6 ELSE m.last_verified_at END,
                           answers = m.answers + 1
                      FROM asked a
                     WHERE m.binding_id = $1 AND m.member = a.member
                       AND (NOT a.present OR m.answers = a.answers)
                 RETURNING a.present, m.answers - 1 <> a.answers AS newer)
               SELECT count(*) FILTER (WHERE NOT present AND newer) AS "stale!" FROM written"#,
            binding,
            &present,
            &present_at,
            &absent,
            &absent_at,
            from_time(at)
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        u64::try_from(stale).map_err(|_| StoreError::Corrupt("a negative count".into()))
    }

    async fn record_sign_in(
        &mut self,
        member: &str,
        provider: &ProviderRef,
        confirmed: &[Uuid],
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO identity_member (binding_id, member, state, last_verified_at, recorded_at)
             SELECT b, $2, 'verified', $3, $3 FROM unnest($1::uuid[]) AS b
             ON CONFLICT (binding_id, member)
             DO UPDATE SET state = 'verified', last_verified_at = EXCLUDED.last_verified_at,
                           answers = identity_member.answers + 1",
            confirmed,
            member,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        sqlx::query!(
            "UPDATE identity_member m SET state = 'unverified', answers = m.answers + 1
               FROM identity_binding b
              WHERE b.id = m.binding_id AND m.member = $1 AND b.organization_id = $2
                AND b.provider = $3 AND NOT (m.binding_id = ANY($4))",
            member,
            provider.organization,
            provider.name,
            confirmed
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
