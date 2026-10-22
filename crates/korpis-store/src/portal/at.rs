// SPDX-License-Identifier: AGPL-3.0-or-later

//! What was begun and registered at a portal (§9.8 of docs/08-identity.md): its sessions and its
//! passkeys, counted before it is deleted and ended and removed when it is.

use crate::PgTx;
use crate::errors::from_sqlx;
use korpis_app::ports::{StoreError, StoreResult};
use uuid::Uuid;

fn count(what: &str, n: i64) -> StoreResult<u64> {
    u64::try_from(n).map_err(|_| StoreError::Corrupt(format!("a negative count of {what}")))
}

pub(super) async fn reach(pool: &sqlx::PgPool, portal: Uuid) -> StoreResult<(u64, u64)> {
    let r = sqlx::query!(
        r#"SELECT (SELECT count(*) FROM passkey WHERE portal_id = $1) AS "passkeys!",
                  (SELECT count(DISTINCT k.account_id) FROM passkey k
                    WHERE k.portal_id = $1
                      AND NOT EXISTS (SELECT 1 FROM password_factor f
                                       WHERE f.account_id = k.account_id)
                      AND NOT EXISTS (SELECT 1 FROM passkey o
                                       WHERE o.account_id = k.account_id
                                         AND o.portal_id IS DISTINCT FROM $1)) AS "only_here!""#,
        portal
    )
    .fetch_one(pool)
    .await
    .map_err(from_sqlx)?;
    Ok((count("passkeys", r.passkeys)?, count("accounts", r.only_here)?))
}

pub(super) async fn end_sessions(tx: &mut PgTx, portal: Uuid) -> StoreResult<u64> {
    let done = sqlx::query!("DELETE FROM user_session WHERE portal_id = $1", portal)
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    Ok(done.rows_affected())
}

pub(super) async fn remove_passkeys(
    tx: &mut PgTx,
    portal: Uuid,
) -> StoreResult<Vec<(Uuid, String)>> {
    let rows = sqlx::query!(
        "DELETE FROM passkey WHERE portal_id = $1 RETURNING account_id, label",
        portal
    )
    .fetch_all(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(rows.into_iter().map(|r| (r.account_id, r.label)).collect())
}
