// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node's proxy proves an order's names with, beyond HTTP-01 (migration 0120, §3.2 of
//! docs/07-networking.md): the TLS-ALPN-01 digests standing while an order runs, carried to the
//! nodes serving the certificate's routes as the HTTP-01 tokens are, and the way a node's proxy
//! last proved the certificate, which its next order tries first.

use crate::errors::from_sqlx;
use crate::{PgStore, PgTx};
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::dns::certificate::{AlpnChallenge, NodeProof};
use uuid::Uuid;

const WAYS: [(NodeProof, &str); 2] =
    [(NodeProof::Http01, "http-01"), (NodeProof::TlsAlpn01, "tls-alpn-01")];

pub(super) async fn alpn_challenges_for(
    tx: &mut PgTx,
    workload: Uuid,
) -> StoreResult<Vec<AlpnChallenge>> {
    let rows = sqlx::query!(
        "SELECT DISTINCT c.name, c.digest
           FROM certificate_alpn_challenge c JOIN route r ON r.certificate_id = c.certificate_id
          WHERE r.workload_id = $1 ORDER BY c.name",
        workload
    )
    .fetch_all(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(rows.into_iter().map(|r| AlpnChallenge { name: r.name, digest: r.digest }).collect())
}

pub(super) async fn clear_alpn_challenges(tx: &mut PgTx, certificate: Uuid) -> StoreResult<()> {
    sqlx::query!("DELETE FROM certificate_alpn_challenge WHERE certificate_id = $1", certificate)
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn put_alpn_challenges(
    tx: &mut PgTx,
    certificate: Uuid,
    challenges: &[AlpnChallenge],
) -> StoreResult<()> {
    clear_alpn_challenges(tx, certificate).await?;
    for c in challenges {
        sqlx::query!(
            "INSERT INTO certificate_alpn_challenge (certificate_id, name, digest)
             VALUES ($1, $2, $3)",
            certificate,
            c.name,
            c.digest
        )
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    }
    Ok(())
}

pub(super) async fn node_proof(store: &PgStore, id: Uuid) -> StoreResult<Option<NodeProof>> {
    let stored: Option<String> =
        sqlx::query_scalar!("SELECT node_proof FROM certificate WHERE id = $1", id)
            .fetch_optional(&store.pool)
            .await
            .map_err(from_sqlx)?
            .flatten();
    stored
        .map(|s| {
            WAYS.iter()
                .find(|(_, n)| *n == s)
                .map(|(w, _)| *w)
                .ok_or_else(|| StoreError::Corrupt(format!("certificate {id}: node proof {s:?}")))
        })
        .transpose()
}

pub(super) async fn set_node_proof(
    tx: &mut PgTx,
    certificate: Uuid,
    way: NodeProof,
) -> StoreResult<()> {
    let named = WAYS.iter().find(|(w, _)| *w == way).map_or("", |(_, n)| n);
    sqlx::query!("UPDATE certificate SET node_proof = $2 WHERE id = $1", certificate, named)
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    Ok(())
}
