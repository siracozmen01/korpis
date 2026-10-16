// SPDX-License-Identifier: AGPL-3.0-or-later

//! `WebAuthn` challenges: issued for one ceremony, taken by the one that answers it.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::account::record::{Ceremony, Challenge};
use std::time::SystemTime;
use uuid::Uuid;

/// How many expired challenges one insert forgets: enough to outpace any rate the API admits
/// ceremonies at, few enough that no insert waits on a large delete.
const FORGET: i64 = 64;

pub(super) async fn insert(tx: &mut PgTx, c: &Challenge, now: SystemTime) -> StoreResult<()> {
    sqlx::query!(
        "DELETE FROM webauthn_challenge
          WHERE id IN (SELECT id FROM webauthn_challenge WHERE expires_at <= $1
                        ORDER BY expires_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
        from_time(now),
        FORGET
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    sqlx::query!(
        "INSERT INTO webauthn_challenge (id, purpose, challenge, account_id, invitation_id,
                                         expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
        c.id,
        c.ceremony.as_str(),
        c.challenge.as_slice(),
        c.account,
        c.invitation,
        from_time(c.expires_at)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn take(
    tx: &mut PgTx,
    id: Uuid,
    now: SystemTime,
) -> StoreResult<Option<Challenge>> {
    let row = sqlx::query!(
        "DELETE FROM webauthn_challenge WHERE id = $1
         RETURNING purpose, challenge, account_id, invitation_id, expires_at",
        id
    )
    .fetch_optional(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    let Some(r) = row else { return Ok(None) };
    let corrupt = |what: &str| StoreError::Corrupt(format!("challenge {id}: {what}"));
    let expires_at = to_time(r.expires_at);
    if expires_at <= now {
        return Ok(None);
    }
    Ok(Some(Challenge {
        id,
        ceremony: Ceremony::parse(&r.purpose).ok_or_else(|| corrupt("purpose"))?,
        challenge: <[u8; 32]>::try_from(r.challenge.as_slice())
            .map_err(|_| corrupt("challenge"))?,
        account: r.account_id,
        invitation: r.invitation_id,
        expires_at,
    }))
}
