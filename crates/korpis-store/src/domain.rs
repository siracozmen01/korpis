// SPDX-License-Identifier: AGPL-3.0-or-later

//! Domains organizations hold (migration 0054). Overlap is a name equal to another or lying
//! beneath it, which is decided on whole labels: `b.example.com` lies beneath `example.com` and
//! `badexample.com` does not.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{DomainRead, DomainWrite, StoreError, StoreResult};
use korpis_core::dns::domain::{Domain, DomainName, Verification};
use std::time::SystemTime;
use uuid::Uuid;

/// The advisory lock serializing verification.
const DOMAIN_LOCK: i64 = 0x6b6f_7270_6973_0002;

struct DomainRow {
    id: Uuid,
    organization_id: Uuid,
    name: String,
    verification: String,
    token: String,
    parent_id: Option<Uuid>,
    verified_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<DomainRow> for Domain {
    type Error = StoreError;

    fn try_from(r: DomainRow) -> StoreResult<Self> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("domain {}: {what}", r.id));
        Ok(Self {
            id: r.id,
            organization: r.organization_id,
            name: DomainName::parse(&r.name).ok_or_else(|| corrupt("name"))?,
            verification: Verification::parse(&r.verification)
                .ok_or_else(|| corrupt("verification"))?,
            token: r.token,
            parent: r.parent_id,
            verified_at: r.verified_at.map(to_time),
            created_at: to_time(r.created_at),
        })
    }
}

fn domains(rows: Vec<DomainRow>) -> StoreResult<Vec<Domain>> {
    rows.into_iter().map(Domain::try_from).collect()
}

impl DomainRead for PgStore {
    async fn domain(&self, id: Uuid) -> StoreResult<Option<Domain>> {
        let row = sqlx::query_as!(
            DomainRow,
            "SELECT id, organization_id, name, verification, token, parent_id, verified_at,
                    created_at
               FROM domain WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(Domain::try_from).transpose()
    }

    async fn domains_of(&self, organization: Uuid, page: Page) -> StoreResult<Vec<Domain>> {
        let rows = sqlx::query_as!(
            DomainRow,
            "SELECT id, organization_id, name, verification, token, parent_id, verified_at,
                    created_at
               FROM domain
              WHERE organization_id = $1 AND ($2::uuid IS NULL OR id > $2)
              ORDER BY id LIMIT $3",
            organization,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        domains(rows)
    }

    async fn verified_covering(&self, name: &DomainName) -> StoreResult<Vec<Domain>> {
        let rows = sqlx::query_as!(
            DomainRow,
            "SELECT id, organization_id, name, verification, token, parent_id, verified_at,
                    created_at
               FROM domain
              WHERE verified_at IS NOT NULL
                AND (name = $1 OR right($1, length(name) + 1) = '.' || name)
              ORDER BY id",
            name.as_str()
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        domains(rows)
    }
}

impl DomainWrite for PgTx {
    async fn lock_domains(&mut self) -> StoreResult<()> {
        sqlx::query!("SELECT pg_advisory_xact_lock($1)", DOMAIN_LOCK)
            .fetch_one(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn verified_overlapping(&mut self, name: &DomainName) -> StoreResult<Vec<Domain>> {
        let rows = sqlx::query_as!(
            DomainRow,
            "SELECT id, organization_id, name, verification, token, parent_id, verified_at,
                    created_at
               FROM domain
              WHERE verified_at IS NOT NULL
                AND (name = $1
                     OR right($1, length(name) + 1) = '.' || name
                     OR right(name, length($1) + 1) = '.' || $1)
              ORDER BY id",
            name.as_str()
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        domains(rows)
    }

    async fn insert_domain(&mut self, d: &Domain) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO domain (id, organization_id, name, verification, token, parent_id,
                                 verified_at, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            d.id,
            d.organization,
            d.name.as_str(),
            d.verification.as_str(),
            d.token,
            d.parent,
            d.verified_at.map(from_time),
            from_time(d.created_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn verify_domain(&mut self, id: Uuid, at: SystemTime) -> StoreResult<()> {
        sqlx::query!("UPDATE domain SET verified_at = $2 WHERE id = $1", id, from_time(at))
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn delete_domain(&mut self, id: Uuid) -> StoreResult<u64> {
        let gone = sqlx::query!(
            "WITH RECURSIVE tree AS (
                 SELECT id FROM domain WHERE id = $1
                 UNION ALL
                 SELECT d.id FROM domain d JOIN tree t ON d.parent_id = t.id)
             DELETE FROM domain WHERE id IN (SELECT id FROM tree)",
            id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(gone.rows_affected())
    }
}
