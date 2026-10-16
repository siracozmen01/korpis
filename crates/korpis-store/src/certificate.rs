// SPDX-License-Identifier: AGPL-3.0-or-later

//! Certificates organizations hold for the ingress (migration 0057). The key is kept only as
//! sealed; nothing here reads it but the node's fetch, which opens it in the vault.

use crate::domain::{RouteRow, routes};
use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{CertificateRead, CertificateWrite, SealedKey, StoreError, StoreResult};
use korpis_core::dns::certificate::{Certificate, Source, State};
use korpis_core::dns::domain::Route;
use korpis_core::workload::endpoint::Served;
use uuid::Uuid;

struct CertificateRow {
    id: Uuid,
    organization_id: Uuid,
    names: Vec<String>,
    source: String,
    chain: String,
    not_before: Option<chrono::DateTime<chrono::Utc>>,
    not_after: Option<chrono::DateTime<chrono::Utc>>,
    state: String,
    reason: Option<String>,
    renew_after: Option<chrono::DateTime<chrono::Utc>>,
    revision: i64,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<CertificateRow> for Certificate {
    type Error = StoreError;

    fn try_from(r: CertificateRow) -> StoreResult<Self> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("certificate {}: {what}", r.id));
        Ok(Self {
            id: r.id,
            organization: r.organization_id,
            names: r.names,
            source: Source::parse(&r.source).ok_or_else(|| corrupt("source"))?,
            chain: r.chain,
            not_before: r.not_before.map(to_time),
            not_after: r.not_after.map(to_time),
            state: State::parse(&r.state).ok_or_else(|| corrupt("state"))?,
            reason: r.reason,
            renew_after: r.renew_after.map(to_time),
            revision: r.revision,
            created_at: to_time(r.created_at),
        })
    }
}

impl CertificateRead for PgStore {
    async fn certificate(&self, id: Uuid) -> StoreResult<Option<Certificate>> {
        let row = sqlx::query_as!(
            CertificateRow,
            "SELECT id, organization_id, names, source, chain, not_before, not_after, state,
                    reason, renew_after, revision, created_at
               FROM certificate WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(Certificate::try_from).transpose()
    }

    async fn routed_with(&self, id: Uuid) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            "SELECT DISTINCT workload_id FROM route WHERE certificate_id = $1 ORDER BY workload_id",
            id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)
    }

    async fn sealed_key(&self, id: Uuid) -> StoreResult<Option<SealedKey>> {
        let row = sqlx::query!(
            r#"SELECT key_sealed AS "sealed!", sealed_by AS "sealed_by!" FROM certificate
                WHERE id = $1 AND key_sealed IS NOT NULL AND sealed_by IS NOT NULL"#,
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(|r| SealedKey { sealed_by: r.sealed_by, sealed: r.sealed }))
    }

    async fn certificates_of(
        &self,
        organization: Uuid,
        page: Page,
    ) -> StoreResult<Vec<Certificate>> {
        let rows = sqlx::query_as!(
            CertificateRow,
            "SELECT id, organization_id, names, source, chain, not_before, not_after, state,
                    reason, renew_after, revision, created_at
               FROM certificate
              WHERE organization_id = $1 AND ($2::uuid IS NULL OR id > $2)
              ORDER BY id LIMIT $3",
            organization,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(Certificate::try_from).collect()
    }
}

impl CertificateWrite for PgTx {
    async fn insert_certificate(
        &mut self,
        c: &Certificate,
        key: Option<&SealedKey>,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO certificate (id, organization_id, names, source, chain, key_sealed,
                                      sealed_by, not_before, not_after, state, reason,
                                      renew_after, revision, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
            c.id,
            c.organization,
            &c.names,
            c.source.as_str(),
            c.chain,
            key.map(|k| k.sealed.as_slice()),
            key.map(|k| k.sealed_by.as_str()),
            c.not_before.map(from_time),
            c.not_after.map(from_time),
            c.state.as_str(),
            c.reason,
            c.renew_after.map(from_time),
            c.revision,
            from_time(c.created_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn lock_certificate(
        &mut self,
        id: Uuid,
        exclusive: bool,
    ) -> StoreResult<Option<Certificate>> {
        // FOR UPDATE and FOR SHARE are two statements: a lock strength is not a parameter.
        let row = if exclusive {
            sqlx::query_as!(
                CertificateRow,
                "SELECT id, organization_id, names, source, chain, not_before, not_after, state,
                        reason, renew_after, revision, created_at
                   FROM certificate WHERE id = $1 FOR UPDATE",
                id
            )
            .fetch_optional(&mut *self.tx)
            .await
        } else {
            sqlx::query_as!(
                CertificateRow,
                "SELECT id, organization_id, names, source, chain, not_before, not_after, state,
                        reason, renew_after, revision, created_at
                   FROM certificate WHERE id = $1 FOR SHARE",
                id
            )
            .fetch_optional(&mut *self.tx)
            .await
        };
        row.map_err(from_sqlx)?.map(Certificate::try_from).transpose()
    }

    async fn replace_certificate(&mut self, c: &Certificate, key: &SealedKey) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE certificate
                SET names = $2, chain = $3, key_sealed = $4, sealed_by = $5, not_before = $6,
                    not_after = $7, state = $8, reason = $9, renew_after = $10, revision = $11
              WHERE id = $1",
            c.id,
            &c.names,
            c.chain,
            key.sealed.as_slice(),
            key.sealed_by,
            c.not_before.map(from_time),
            c.not_after.map(from_time),
            c.state.as_str(),
            c.reason,
            c.renew_after.map(from_time),
            c.revision
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn delete_certificate(&mut self, id: Uuid) -> StoreResult<()> {
        sqlx::query!("DELETE FROM certificate WHERE id = $1", id)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn served_for(&mut self, workload: Uuid) -> StoreResult<Vec<(String, Served)>> {
        let rows = sqlx::query!(
            "SELECT DISTINCT r.endpoint, c.id, c.revision
               FROM route r JOIN certificate c ON c.id = r.certificate_id
              WHERE r.workload_id = $1 AND c.key_sealed IS NOT NULL
              ORDER BY r.endpoint",
            workload
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(rows
            .into_iter()
            .map(|r| (r.endpoint, Served { id: r.id, revision: r.revision }))
            .collect())
    }

    async fn routes_served_by(&mut self, id: Uuid) -> StoreResult<Vec<Route>> {
        let rows = sqlx::query_as!(
            RouteRow,
            "SELECT name, organization_id, workload_id, endpoint, certificate_id FROM route
              WHERE certificate_id = $1 ORDER BY name",
            id
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        routes(rows)
    }
}
