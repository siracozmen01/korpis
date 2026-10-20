// SPDX-License-Identifier: AGPL-3.0-or-later

//! Presentations and tickets (migration 0097).

use crate::PgTx;
use crate::binding::{memberships, millis};
use crate::errors::from_sqlx;
use crate::stored::{from_json, from_time, principal, to_json, to_time};
use chrono::{DateTime, Utc};
use korpis_app::ports::{AssertionWrite, StoreError, StoreResult};
use korpis_core::authz::binding::{IdentityBinding, ProviderRef};
use korpis_core::authz::ticket::Ticket;
use korpis_core::authz::{Action, Kind, ObjectRef};
use korpis_core::principal::SetName;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use uuid::Uuid;

/// One set an assertion named.
#[derive(Debug, Serialize, Deserialize)]
struct SetDoc {
    realm: String,
    member_of: String,
}

struct TicketRow {
    id: Uuid,
    relay: String,
    subject: String,
    sets: serde_json::Value,
    organization_id: Uuid,
    provider: String,
    presentation: String,
    action: String,
    object_kind: String,
    object_id: Uuid,
    expires_at: DateTime<Utc>,
}

impl TicketRow {
    fn into_domain(self) -> StoreResult<Ticket> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("ticket {what}"));
        let sets: Vec<SetDoc> = from_json("ticket sets", self.sets)?;
        let kind = Kind::parse(&self.object_kind).ok_or_else(|| corrupt("object kind"))?;
        Ok(Ticket {
            id: self.id,
            relay: principal(&self.relay)?,
            subject: principal(&self.subject)?,
            sets: sets
                .into_iter()
                .map(|s| SetName { realm: s.realm, member_of: s.member_of })
                .collect(),
            provider: ProviderRef { organization: self.organization_id, name: self.provider },
            presentation: self.presentation,
            action: Action::parse(&self.action).map_err(|_| corrupt("action"))?,
            object: ObjectRef::new(kind, self.object_id),
            expires_at: to_time(self.expires_at),
        })
    }
}

impl AssertionWrite for PgTx {
    async fn accept_presentation(
        &mut self,
        provider: &ProviderRef,
        presentation: &str,
        at: SystemTime,
    ) -> StoreResult<bool> {
        let accepted = sqlx::query!(
            "INSERT INTO identity_presentation (organization_id, provider, presentation,
                                                accepted_at)
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            provider.organization,
            provider.name,
            presentation,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(accepted == 1)
    }

    async fn bind_at_first_use(&mut self, b: &IdentityBinding) -> StoreResult<bool> {
        let t = &b.terms;
        let inserted = sqlx::query!(
            "INSERT INTO identity_binding (id, organization_id, provider, subject, is_set, origin,
                    require_membership, grace_ms, reverify_ms, allows_irreversible, state,
                    last_verified_at, version, made_by, made_at, verifications)
             SELECT $1, $2, $3, $4, false, 'first_use', $5, $6, $7, false, 'verified', $8, 1, $9,
                    $10, 0
              WHERE NOT EXISTS (SELECT 1 FROM identity_binding WHERE subject = $4)
             ON CONFLICT (subject) WHERE retired_at IS NULL DO NOTHING",
            b.id,
            b.provider.organization,
            b.provider.name,
            b.subject.key(),
            memberships(t)?,
            millis("binding grace", t.grace)?,
            millis("binding interval", t.reverify_interval)?,
            b.last_verified_at.map(from_time),
            b.made_by.key(),
            from_time(b.made_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(inserted == 1)
    }

    async fn insert_ticket(&mut self, t: &Ticket, digest: &[u8; 32]) -> StoreResult<()> {
        let sets: Vec<SetDoc> = t
            .sets
            .iter()
            .map(|s| SetDoc { realm: s.realm.clone(), member_of: s.member_of.clone() })
            .collect();
        sqlx::query!(
            "INSERT INTO identity_ticket (id, digest, relay, subject, sets, organization_id,
                                          provider, presentation, action, object_kind,
                                          object_id, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            t.id,
            &digest[..],
            t.relay.key(),
            t.subject.key(),
            to_json("ticket sets", &sets)?,
            t.provider.organization,
            t.provider.name,
            t.presentation,
            t.action.as_str(),
            t.object.kind.as_str(),
            t.object.id,
            from_time(t.expires_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn redeem_ticket(
        &mut self,
        digest: &[u8; 32],
        relay: &str,
        now: SystemTime,
    ) -> StoreResult<Option<Ticket>> {
        let row = sqlx::query_as!(
            TicketRow,
            "UPDATE identity_ticket SET used_at = $3
              WHERE digest = $1 AND relay = $2 AND used_at IS NULL AND expires_at > $3
          RETURNING id, relay, subject, sets, organization_id, provider, presentation, action,
                    object_kind, object_id, expires_at",
            &digest[..],
            relay,
            from_time(now)
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(TicketRow::into_domain).transpose()
    }

    async fn forget_assertions(&mut self, before: SystemTime) -> StoreResult<u64> {
        let at = from_time(before);
        let presentations =
            sqlx::query!("DELETE FROM identity_presentation WHERE accepted_at < $1", at)
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?
                .rows_affected();
        let tickets = sqlx::query!("DELETE FROM identity_ticket WHERE expires_at < $1", at)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?
            .rows_affected();
        Ok(presentations + tickets)
    }
}
