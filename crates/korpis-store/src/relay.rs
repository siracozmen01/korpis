// SPDX-License-Identifier: AGPL-3.0-or-later

//! Relays that run as their own process, and the tokens that enroll them (§3.1 of
//! docs/02-architecture.md).

use crate::errors::from_sqlx;
use crate::stored::{from_time, principal, to_time};
use crate::tenancy::name;
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::ports::{RelayEnrollment, RelayRead, RelayWrite, StoreError, StoreResult};
use korpis_core::relay::{Relay, RelayUrl};
use std::time::SystemTime;
use uuid::Uuid;

struct RelayRow {
    id: Uuid,
    name: String,
    url: String,
    joined_at: DateTime<Utc>,
    certificate_expires_at: Option<DateTime<Utc>>,
    removed_at: Option<DateTime<Utc>>,
}

fn url(s: &str) -> StoreResult<RelayUrl> {
    RelayUrl::parse(s).map_err(|e| StoreError::Corrupt(e.to_string()))
}

impl RelayRow {
    fn into_domain(self) -> StoreResult<Relay> {
        Ok(Relay {
            id: self.id,
            name: name(self.name)?,
            url: url(&self.url)?,
            joined_at: to_time(self.joined_at),
            certificate_expires_at: self.certificate_expires_at.map(to_time),
            removed_at: self.removed_at.map(to_time),
        })
    }
}

impl RelayRead for PgStore {
    async fn relays(&self) -> StoreResult<Vec<Relay>> {
        let rows = sqlx::query_as!(
            RelayRow,
            "SELECT id, name, url, joined_at, certificate_expires_at, removed_at
               FROM relay ORDER BY name"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(RelayRow::into_domain).collect()
    }

    async fn relay(&self, id: Uuid) -> StoreResult<Option<Relay>> {
        let row = sqlx::query_as!(
            RelayRow,
            "SELECT id, name, url, joined_at, certificate_expires_at, removed_at
               FROM relay WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(RelayRow::into_domain).transpose()
    }

    async fn relay_key(&self, id: Uuid) -> StoreResult<Option<Vec<u8>>> {
        sqlx::query_scalar!("SELECT public_key FROM relay WHERE id = $1", id)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)
    }

    async fn relay_enrollment(&self, token: Uuid) -> StoreResult<Option<RelayEnrollment>> {
        let row = sqlx::query!(
            "SELECT token_id, grant_id, organization_id, relay_name, relay_url, created_by,
                    created_at, expires_at, relay_id
               FROM relay_enrollment WHERE token_id = $1",
            token
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            Ok(RelayEnrollment {
                token: r.token_id,
                grant: r.grant_id,
                organization: r.organization_id,
                relay_name: name(r.relay_name)?,
                relay_url: url(&r.relay_url)?,
                created_by: principal(&r.created_by)?,
                created_at: to_time(r.created_at),
                expires_at: to_time(r.expires_at),
                relay: r.relay_id,
            })
        })
        .transpose()
    }
}

impl RelayWrite for PgTx {
    async fn insert_relay_enrollment(&mut self, e: &RelayEnrollment) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO relay_enrollment (token_id, grant_id, organization_id, relay_name,
                                           relay_url, created_by, created_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            e.token,
            e.grant,
            e.organization,
            e.relay_name.as_str(),
            e.relay_url.as_str(),
            e.created_by.key(),
            from_time(e.created_at),
            from_time(e.expires_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn join_relay(&mut self, r: &Relay, public_key: &[u8], token: Uuid) -> StoreResult<bool> {
        sqlx::query!(
            "INSERT INTO relay (id, name, url, public_key, joined_at)
             VALUES ($1, $2, $3, $4, $5)",
            r.id,
            r.name.as_str(),
            r.url.as_str(),
            public_key,
            from_time(r.joined_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let done = sqlx::query!(
            "UPDATE relay_enrollment SET relay_id = $2, joined_at = $3
              WHERE token_id = $1 AND relay_id IS NULL",
            token,
            r.id,
            from_time(r.joined_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn record_relay_certificate(
        &mut self,
        relay: Uuid,
        expires_at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE relay SET certificate_expires_at = $2 WHERE id = $1",
            relay,
            from_time(expires_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn remove_relay(&mut self, relay: Uuid, at: SystemTime) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE relay SET removed_at = $2 WHERE id = $1 AND removed_at IS NULL",
            relay,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
