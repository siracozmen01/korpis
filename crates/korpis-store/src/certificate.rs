// SPDX-License-Identifier: AGPL-3.0-or-later

//! Certificates organizations hold for the ingress (migration 0057). The key is kept only as
//! sealed; nothing here reads it but the node's fetch, which opens it in the vault.

use crate::domain::{RouteRow, routes};
use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{
    CertificateRead, CertificateWrite, Expiry, Exposure, HttpOffer, PendingRevocation, SealedKey,
    StandingRecord, StoreError, StoreResult,
};
use korpis_core::dns::certificate::{
    Certificate, ExpiryAlert, HttpChallenge, Owner, Source, State,
};
use korpis_core::dns::domain::Route;
use korpis_core::event::Severity;
use korpis_core::workload::endpoint::Served;
use std::time::SystemTime;
use uuid::Uuid;

mod expiry;
mod exposure;
mod issue;

pub(crate) struct CertificateRow {
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
    workload_id: Option<Uuid>,
    endpoint: Option<String>,
    portal_id: Option<Uuid>,
    attempts: i32,
    next_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    key_id: Option<String>,
    exposed: bool,
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
            owner: r
                .workload_id
                .zip(r.endpoint)
                .map(|(workload, endpoint)| Owner { workload, endpoint }),
            portal: r.portal_id,
            attempts: u32::try_from(r.attempts).map_err(|_| corrupt("attempts"))?,
            next_attempt: r.next_attempt_at.map(to_time),
            key_id: r.key_id,
            exposed: r.exposed,
        })
    }
}

impl CertificateRead for PgStore {
    async fn certificate(&self, id: Uuid) -> StoreResult<Option<Certificate>> {
        let row = sqlx::query_as!(
            CertificateRow,
            "SELECT id, organization_id, names, source, chain, not_before, not_after, state,
                    reason, renew_after, revision, created_at, workload_id, endpoint, portal_id,
                    attempts, next_attempt_at, key_id, exposed
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
                    reason, renew_after, revision, created_at, workload_id, endpoint, portal_id,
                    attempts, next_attempt_at, key_id, exposed
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

    async fn certificate_expiries(&self) -> StoreResult<Vec<Expiry>> {
        expiry::certificate_expiries(&self.pool).await
    }

    async fn routed_certificates(&self, workload: Uuid) -> StoreResult<Vec<(String, Uuid)>> {
        let rows = sqlx::query!(
            r#"SELECT DISTINCT endpoint, certificate_id AS "certificate!" FROM route
                WHERE workload_id = $1 AND certificate_id IS NOT NULL ORDER BY endpoint"#,
            workload
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(|r| (r.endpoint, r.certificate)).collect())
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
                                      renew_after, revision, created_at, workload_id, endpoint,
                                      key_id, portal_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                     $18)",
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
            from_time(c.created_at),
            c.owner.as_ref().map(|o| o.workload),
            c.owner.as_ref().map(|o| o.endpoint.as_str()),
            c.key_id,
            c.portal
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
                        reason, renew_after, revision, created_at, workload_id, endpoint,
                        portal_id, attempts, next_attempt_at, key_id, exposed
                   FROM certificate WHERE id = $1 FOR UPDATE",
                id
            )
            .fetch_optional(&mut *self.tx)
            .await
        } else {
            sqlx::query_as!(
                CertificateRow,
                "SELECT id, organization_id, names, source, chain, not_before, not_after, state,
                        reason, renew_after, revision, created_at, workload_id, endpoint,
                        portal_id, attempts, next_attempt_at, key_id, exposed
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
                    not_after = $7, state = $8, reason = $9, renew_after = $10, revision = $11,
                    attempts = $12, next_attempt_at = $13, issuing_until = NULL, key_id = $14,
                    exposed = $15,
                    expiry_alert = CASE WHEN not_after IS DISTINCT FROM $7 THEN NULL
                                        ELSE expiry_alert END
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
            c.revision,
            i32::try_from(c.attempts).unwrap_or(i32::MAX),
            c.next_attempt.map(from_time),
            c.key_id,
            c.exposed
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        issue::clear_challenges(self, c.id).await
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

    async fn acme_certificate(
        &mut self,
        organization: Uuid,
        owner: &Owner,
        names: &[String],
        at: SystemTime,
    ) -> StoreResult<Uuid> {
        issue::acme_certificate(self, organization, owner, (names, at)).await
    }

    async fn challenges_for(&mut self, workload: Uuid) -> StoreResult<Vec<HttpChallenge>> {
        issue::challenges_for(self, workload).await
    }

    async fn put_challenges(
        &mut self,
        certificate: Uuid,
        challenges: &[(String, HttpOffer)],
    ) -> StoreResult<()> {
        issue::put_challenges(self, certificate, challenges).await
    }

    async fn take_due(
        &mut self,
        now: SystemTime,
        until: SystemTime,
    ) -> StoreResult<Option<Certificate>> {
        issue::take_due(self, now, until).await
    }

    async fn record_attempt(&mut self, c: &Certificate) -> StoreResult<()> {
        issue::record_attempt(self, c).await
    }

    async fn rearm_certificates(&mut self) -> StoreResult<u64> {
        issue::rearm(self).await
    }

    async fn remove_orphans(&mut self) -> StoreResult<u64> {
        issue::remove_orphans(self).await
    }

    async fn alert_certificate(
        &mut self,
        alert: &ExpiryAlert,
        expected: Option<Severity>,
    ) -> StoreResult<bool> {
        expiry::alert_certificate(self, alert, expected).await
    }

    async fn record_exposure(&mut self, e: &Exposure) -> StoreResult<()> {
        exposure::record_exposure(self, e).await
    }

    async fn expose_certificates(&mut self, node: Uuid, at: SystemTime) -> StoreResult<u64> {
        exposure::expose_certificates(self, node, at).await
    }

    async fn pending_revocations(&mut self) -> StoreResult<Vec<PendingRevocation>> {
        exposure::pending_revocations(self).await
    }

    async fn record_revocation(
        &mut self,
        r: &PendingRevocation,
        outcome: &str,
        at: SystemTime,
    ) -> StoreResult<()> {
        exposure::record_revocation(self, r, outcome, at).await
    }

    async fn put_record(
        &mut self,
        certificate: Uuid,
        provider: &str,
        owner: &str,
    ) -> StoreResult<()> {
        issue::put_record(self, certificate, provider, owner).await
    }

    async fn drop_record(
        &mut self,
        certificate: Uuid,
        provider: &str,
        owner: &str,
    ) -> StoreResult<()> {
        issue::drop_record(self, certificate, provider, owner).await
    }

    async fn standing_records(
        &mut self,
        certificate: Option<Uuid>,
        now: SystemTime,
    ) -> StoreResult<Vec<StandingRecord>> {
        issue::standing_records(self, certificate, now).await
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
