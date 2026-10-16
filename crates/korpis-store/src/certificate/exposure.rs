// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which nodes were given which certificate keys (migration 0062), and what removing one as
//! compromised does to them: each certificate whose current key it was given is marked exposed,
//! an `acme` one made due for a new key at once, and each told to its organization as a
//! `certificate.exposed` event; every chain of an `acme` certificate it was given waits to be
//! revoked.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::from_time;
use korpis_app::ports::{Exposure, PendingRevocation, StoreResult};
use std::time::SystemTime;
use uuid::Uuid;

pub(super) async fn record_exposure(tx: &mut PgTx, e: &Exposure) -> StoreResult<()> {
    sqlx::query!(
        "INSERT INTO certificate_exposure (certificate_id, node_id, revision, key_id, chain,
                                           given_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT DO NOTHING",
        e.certificate,
        e.node,
        e.revision,
        e.key_id,
        e.chain,
        from_time(e.at)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn expose_certificates(
    tx: &mut PgTx,
    node: Uuid,
    at: SystemTime,
) -> StoreResult<u64> {
    sqlx::query!("UPDATE node SET compromised_at = $2 WHERE id = $1", node, from_time(at))
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    let exposed = sqlx::query!(
        "UPDATE certificate c
            SET exposed = true,
                renew_after = CASE WHEN c.source = 'acme' THEN $2 ELSE c.renew_after END,
                next_attempt_at = CASE WHEN c.source = 'acme' THEN NULL ELSE c.next_attempt_at END
          WHERE EXISTS (SELECT 1 FROM certificate_exposure e
                         WHERE e.certificate_id = c.id AND e.node_id = $1
                           AND e.key_id = c.key_id)
         RETURNING c.id, c.organization_id, c.names, c.source",
        node,
        from_time(at)
    )
    .fetch_all(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    // The issuer gives each exposed acme certificate a new key now, not at its next look.
    sqlx::query!("SELECT pg_notify('korpis_certificate', '')")
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    for c in &exposed {
        let detail = serde_json::json!({
            "certificate": c.id.to_string(),
            "names": c.names,
            "source": c.source,
            "node": node.to_string(),
        });
        sqlx::query!(
            "SELECT korpis_append_event($1, $2, 'certificate.exposed', 'critical', $3, NULL,
                                        NULL, $4)",
            format!("certificate:{}", c.id),
            from_time(at),
            c.organization_id,
            detail
        )
        .fetch_one(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    }
    Ok(u64::try_from(exposed.len()).unwrap_or(u64::MAX))
}

pub(super) async fn pending_revocations(tx: &mut PgTx) -> StoreResult<Vec<PendingRevocation>> {
    let rows = sqlx::query!(
        "SELECT e.certificate_id, e.node_id, e.revision, e.chain
           FROM certificate_exposure e
           JOIN node n ON n.id = e.node_id
           JOIN certificate c ON c.id = e.certificate_id
          WHERE n.compromised_at IS NOT NULL AND e.revoked_at IS NULL AND c.source = 'acme'
          ORDER BY e.given_at"
    )
    .fetch_all(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(rows
        .into_iter()
        .map(|r| PendingRevocation {
            certificate: r.certificate_id,
            node: r.node_id,
            revision: r.revision,
            chain: r.chain,
        })
        .collect())
}

pub(super) async fn record_revocation(
    tx: &mut PgTx,
    r: &PendingRevocation,
    outcome: &str,
    at: SystemTime,
) -> StoreResult<()> {
    sqlx::query!(
        "UPDATE certificate_exposure SET revoked_at = $4, revocation = $5
          WHERE certificate_id = $1 AND node_id = $2 AND revision = $3",
        r.certificate,
        r.node,
        r.revision,
        from_time(at),
        outcome
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}
