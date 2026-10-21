// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the issuer of `acme` certificates reads and writes (migration 0059). A certificate due is
//! taken under a lease, `issuing_until`, with `FOR UPDATE SKIP LOCKED`, so two issuers never take
//! one; an orphan is removed only if no claim holds it, a claim taking the certificate it reuses
//! `FOR SHARE`, so a claim never names a certificate removed beneath it.

use super::CertificateRow;
use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::from_time;
use korpis_app::ports::{HttpOffer, StandingRecord, StoreResult};
use korpis_core::dns::certificate::{Certificate, HttpChallenge, Owner};
use std::time::SystemTime;
use uuid::Uuid;

pub(super) async fn acme_certificate(
    tx: &mut PgTx,
    organization: Uuid,
    owner: &Owner,
    (names, at): (&[String], SystemTime),
) -> StoreResult<Uuid> {
    let held = sqlx::query_scalar!(
        "SELECT id FROM certificate
          WHERE source = 'acme' AND workload_id = $1 AND endpoint = $2 AND names = $3
          FOR SHARE",
        owner.workload,
        owner.endpoint,
        names
    )
    .fetch_optional(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    if let Some(id) = held {
        return Ok(id);
    }
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO certificate (id, organization_id, names, source, chain, state, revision,
                                  created_at, workload_id, endpoint)
         VALUES ($1, $2, $3, 'acme', '', 'pending', 0, $4, $5, $6)",
        id,
        organization,
        names,
        from_time(at),
        owner.workload,
        owner.endpoint
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(id)
}

pub(super) async fn challenges_for(
    tx: &mut PgTx,
    workload: Uuid,
) -> StoreResult<Vec<HttpChallenge>> {
    let rows = sqlx::query!(
        "SELECT DISTINCT c.token, c.key_authorization
           FROM certificate_challenge c JOIN route r ON r.certificate_id = c.certificate_id
          WHERE r.workload_id = $1 ORDER BY c.token",
        workload
    )
    .fetch_all(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(rows
        .into_iter()
        .map(|r| HttpChallenge { token: r.token, key_authorization: r.key_authorization })
        .collect())
}

pub(super) async fn clear_challenges(tx: &mut PgTx, certificate: Uuid) -> StoreResult<()> {
    sqlx::query!("DELETE FROM certificate_challenge WHERE certificate_id = $1", certificate)
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn put_challenges(
    tx: &mut PgTx,
    certificate: Uuid,
    challenges: &[(String, HttpOffer)],
) -> StoreResult<()> {
    clear_challenges(tx, certificate).await?;
    for (name, c) in challenges {
        sqlx::query!(
            "INSERT INTO certificate_challenge (certificate_id, token, name, key_authorization)
             VALUES ($1, $2, $3, $4)",
            certificate,
            c.token,
            name,
            c.key_authorization
        )
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    }
    Ok(())
}

pub(super) async fn take_due(
    tx: &mut PgTx,
    now: SystemTime,
    until: SystemTime,
) -> StoreResult<Option<Certificate>> {
    let row = sqlx::query_as!(
        CertificateRow,
        "UPDATE certificate SET issuing_until = $2
          WHERE id = (SELECT id FROM certificate
                       WHERE source = 'acme'
                         AND (issuing_until IS NULL OR issuing_until < $1)
                         AND (next_attempt_at IS NULL OR next_attempt_at <= $1)
                         AND (state <> 'issued' OR renew_after <= $1)
                       ORDER BY next_attempt_at NULLS FIRST, id
                       LIMIT 1 FOR UPDATE SKIP LOCKED)
          RETURNING id, organization_id, names, source, chain, not_before, not_after, state,
                    reason, renew_after, revision, created_at, workload_id, endpoint, portal_id,
                    attempts, next_attempt_at, key_id, exposed",
        from_time(now),
        from_time(until)
    )
    .fetch_optional(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    row.map(Certificate::try_from).transpose()
}

pub(super) async fn record_attempt(tx: &mut PgTx, c: &Certificate) -> StoreResult<()> {
    sqlx::query!(
        "UPDATE certificate
            SET state = $2, reason = $3, attempts = $4, next_attempt_at = $5, issuing_until = NULL
          WHERE id = $1",
        c.id,
        c.state.as_str(),
        c.reason,
        i32::try_from(c.attempts).unwrap_or(i32::MAX),
        c.next_attempt.map(from_time)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    clear_challenges(tx, c.id).await
}

pub(super) async fn rearm(tx: &mut PgTx) -> StoreResult<u64> {
    let done = sqlx::query!(
        "UPDATE certificate SET next_attempt_at = NULL, issuing_until = NULL
          WHERE source = 'acme' AND state <> 'issued'"
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(done.rows_affected())
}

pub(super) async fn remove_orphans(tx: &mut PgTx) -> StoreResult<u64> {
    let done = sqlx::query!(
        "DELETE FROM certificate
          WHERE id IN (SELECT c.id FROM certificate c
                        WHERE c.source = 'acme'
                          AND NOT EXISTS (SELECT 1 FROM route r WHERE r.certificate_id = c.id)
                          AND NOT EXISTS (SELECT 1 FROM portal p WHERE p.certificate_id = c.id)
                          AND NOT EXISTS (SELECT 1 FROM endpoint e WHERE e.certificate_id = c.id)
                          AND NOT EXISTS (SELECT 1 FROM certificate_record d
                                           WHERE d.certificate_id = c.id)
                          AND NOT EXISTS (SELECT 1 FROM certificate_exposure x
                                            JOIN node n ON n.id = x.node_id
                                           WHERE x.certificate_id = c.id
                                             AND n.compromised_at IS NOT NULL
                                             AND x.revoked_at IS NULL)
                        FOR UPDATE SKIP LOCKED)"
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(done.rows_affected())
}

pub(super) async fn put_record(
    tx: &mut PgTx,
    certificate: Uuid,
    provider: &str,
    owner: &str,
) -> StoreResult<()> {
    sqlx::query!(
        "INSERT INTO certificate_record (certificate_id, provider, owner) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING",
        certificate,
        provider,
        owner
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn drop_record(
    tx: &mut PgTx,
    certificate: Uuid,
    provider: &str,
    owner: &str,
) -> StoreResult<()> {
    sqlx::query!(
        "DELETE FROM certificate_record
          WHERE certificate_id = $1 AND provider = $2 AND owner = $3",
        certificate,
        provider,
        owner
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn standing_records(
    tx: &mut PgTx,
    certificate: Option<Uuid>,
    now: SystemTime,
) -> StoreResult<Vec<StandingRecord>> {
    let rows = sqlx::query!(
        "SELECT d.certificate_id, c.organization_id, d.provider, d.owner
           FROM certificate_record d JOIN certificate c ON c.id = d.certificate_id
          WHERE CASE WHEN $1::uuid IS NULL
                     THEN c.issuing_until IS NULL OR c.issuing_until < $2
                     ELSE d.certificate_id = $1 END
          ORDER BY d.certificate_id, d.provider, d.owner",
        certificate,
        from_time(now)
    )
    .fetch_all(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(rows
        .into_iter()
        .map(|r| StandingRecord {
            certificate: r.certificate_id,
            organization: r.organization_id,
            provider: r.provider,
            owner: r.owner,
        })
        .collect())
}
