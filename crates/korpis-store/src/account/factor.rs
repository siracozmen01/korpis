// SPDX-License-Identifier: AGPL-3.0-or-later

//! Factors: passkeys, the password, TOTP, and recovery codes.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::from_time;
use korpis_app::ports::{SealedKey, StoreError, StoreResult};
use korpis_core::account::record::Passkey;
use std::time::SystemTime;
use uuid::Uuid;

/// A step as stored: steps are seconds over thirty since 1970, far inside `i64`.
fn step_of(step: u64) -> StoreResult<i64> {
    i64::try_from(step).map_err(|_| StoreError::Failed(format!("a TOTP step {step} too late")))
}

pub(super) async fn insert_passkey(tx: &mut PgTx, p: &Passkey) -> StoreResult<()> {
    let algorithm = i32::try_from(p.algorithm.cose())
        .map_err(|_| StoreError::Failed(format!("algorithm {}", p.algorithm.cose())))?;
    sqlx::query!(
        "INSERT INTO passkey (credential_id, account_id, public_key, algorithm, sign_count, aaguid,
                              backup_eligible, label, created_at, last_used_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        p.credential_id,
        p.account,
        p.public_key,
        algorithm,
        i64::from(p.sign_count),
        p.aaguid.as_slice(),
        p.backup_eligible,
        p.label,
        from_time(p.created_at),
        p.last_used_at.map(from_time)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn passkey_used(
    tx: &mut PgTx,
    credential_id: &[u8],
    seen: u32,
    count: u32,
    at: SystemTime,
) -> StoreResult<bool> {
    let done = sqlx::query!(
        "UPDATE passkey SET sign_count = $3, last_used_at = $4
          WHERE credential_id = $1 AND sign_count = $2",
        credential_id,
        i64::from(seen),
        i64::from(count),
        from_time(at)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(done.rows_affected() == 1)
}

pub(super) async fn remove_passkey(
    tx: &mut PgTx,
    account: Uuid,
    credential_id: &[u8],
) -> StoreResult<bool> {
    let done = sqlx::query!(
        "DELETE FROM passkey WHERE account_id = $1 AND credential_id = $2",
        account,
        credential_id
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(done.rows_affected() == 1)
}

pub(super) async fn set_password(
    tx: &mut PgTx,
    account: Uuid,
    phc: &str,
    at: SystemTime,
) -> StoreResult<()> {
    sqlx::query!(
        "INSERT INTO password_factor (account_id, phc, updated_at) VALUES ($1, $2, $3)
         ON CONFLICT (account_id) DO UPDATE SET phc = $2, updated_at = $3",
        account,
        phc,
        from_time(at)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn remove_password(tx: &mut PgTx, account: Uuid) -> StoreResult<bool> {
    let done = sqlx::query!("DELETE FROM password_factor WHERE account_id = $1", account)
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    Ok(done.rows_affected() == 1)
}

pub(super) async fn set_totp(
    tx: &mut PgTx,
    account: Uuid,
    sealed: &SealedKey,
    step: u64,
) -> StoreResult<()> {
    sqlx::query!(
        "INSERT INTO totp_factor (account_id, sealed_by, sealed, last_step) VALUES ($1, $2, $3, $4)
         ON CONFLICT (account_id) DO UPDATE
            SET sealed_by = $2, sealed = $3, last_step = $4, created_at = now()",
        account,
        sealed.sealed_by,
        sealed.sealed,
        step_of(step)?
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn totp_used(tx: &mut PgTx, account: Uuid, step: u64) -> StoreResult<bool> {
    let done = sqlx::query!(
        "UPDATE totp_factor SET last_step = $2
          WHERE account_id = $1 AND (last_step IS NULL OR last_step < $2)",
        account,
        step_of(step)?
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(done.rows_affected() == 1)
}

pub(super) async fn remove_totp(tx: &mut PgTx, account: Uuid) -> StoreResult<bool> {
    let done = sqlx::query!("DELETE FROM totp_factor WHERE account_id = $1", account)
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    Ok(done.rows_affected() == 1)
}

pub(super) async fn replace_recovery_codes(
    tx: &mut PgTx,
    account: Uuid,
    digests: &[[u8; 32]],
) -> StoreResult<()> {
    sqlx::query!("DELETE FROM recovery_code WHERE account_id = $1", account)
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    let digests: Vec<Vec<u8>> = digests.iter().map(|d| d.to_vec()).collect();
    sqlx::query!(
        "INSERT INTO recovery_code (account_id, digest) SELECT $1, d FROM unnest($2::bytea[]) d",
        account,
        &digests
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn use_recovery_code(
    tx: &mut PgTx,
    account: Uuid,
    digest: &[u8; 32],
    at: SystemTime,
) -> StoreResult<bool> {
    let done = sqlx::query!(
        "UPDATE recovery_code SET used_at = $3
          WHERE account_id = $1 AND digest = $2 AND used_at IS NULL",
        account,
        digest.as_slice(),
        from_time(at)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(done.rows_affected() == 1)
}
