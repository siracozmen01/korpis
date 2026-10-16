// SPDX-License-Identifier: AGPL-3.0-or-later

//! Accounts, invitations, and factors (migration 0067). Secrets are kept only as digests or
//! sealed, and every single-use thing is spent by a conditional update, so two requests racing
//! for it cannot both succeed.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{
    AccountRead, AccountWrite, SealedKey, StoreError, StoreResult, TotpFactor,
};
use korpis_core::account::record::{Account, Challenge, Factors, Invitation, Passkey};
use korpis_core::webauthn::Algorithm;
use std::time::SystemTime;
use uuid::Uuid;

mod challenge;
mod factor;

struct AccountRow {
    id: Uuid,
    display_name: String,
    name: Option<String>,
    created_by: String,
    created_at: chrono::DateTime<chrono::Utc>,
    disabled_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<AccountRow> for Account {
    fn from(r: AccountRow) -> Self {
        Self {
            id: r.id,
            display_name: r.display_name,
            name: r.name,
            created_by: r.created_by,
            created_at: to_time(r.created_at),
            disabled_at: r.disabled_at.map(to_time),
        }
    }
}

pub(crate) struct PasskeyRow {
    credential_id: Vec<u8>,
    account_id: Uuid,
    public_key: Vec<u8>,
    algorithm: i32,
    sign_count: i64,
    aaguid: Vec<u8>,
    backup_eligible: bool,
    label: String,
    created_at: chrono::DateTime<chrono::Utc>,
    last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl TryFrom<PasskeyRow> for Passkey {
    type Error = StoreError;

    fn try_from(r: PasskeyRow) -> StoreResult<Self> {
        let corrupt =
            |what: &str| StoreError::Corrupt(format!("passkey of {}: {what}", r.account_id));
        let algorithm = [Algorithm::Es256, Algorithm::EdDsa, Algorithm::Rs256]
            .into_iter()
            .find(|a| a.cose() == i64::from(r.algorithm))
            .ok_or_else(|| corrupt("algorithm"))?;
        Ok(Self {
            credential_id: r.credential_id,
            account: r.account_id,
            public_key: r.public_key,
            algorithm,
            sign_count: u32::try_from(r.sign_count).map_err(|_| corrupt("sign count"))?,
            aaguid: <[u8; 16]>::try_from(r.aaguid.as_slice()).map_err(|_| corrupt("aaguid"))?,
            backup_eligible: r.backup_eligible,
            label: r.label,
            created_at: to_time(r.created_at),
            last_used_at: r.last_used_at.map(to_time),
        })
    }
}

/// A count as stored, which no table here can make negative or past `u32`.
fn count(n: Option<i64>) -> u32 {
    n.and_then(|n| u32::try_from(n).ok()).unwrap_or(0)
}

impl AccountRead for PgStore {
    async fn account(&self, id: Uuid) -> StoreResult<Option<Account>> {
        let row = sqlx::query_as!(
            AccountRow,
            "SELECT id, display_name, name, created_by, created_at, disabled_at
               FROM account WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(Account::from))
    }

    async fn account_named(&self, name: &str) -> StoreResult<Option<Account>> {
        let row = sqlx::query_as!(
            AccountRow,
            "SELECT id, display_name, name, created_by, created_at, disabled_at
               FROM account WHERE name = $1",
            name
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(Account::from))
    }

    async fn invitation_by_digest(&self, digest: &[u8; 32]) -> StoreResult<Option<Invitation>> {
        let row = sqlx::query!(
            "SELECT id, account_id, issued_by, expires_at, passkey_only, redeemed_at
               FROM invitation WHERE secret_hash = $1",
            digest.as_slice()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(|r| Invitation {
            id: r.id,
            account: r.account_id,
            issued_by: r.issued_by,
            expires_at: to_time(r.expires_at),
            passkey_only: r.passkey_only,
            redeemed_at: r.redeemed_at.map(to_time),
        }))
    }

    async fn factors(&self, account: Uuid) -> StoreResult<Factors> {
        let r = sqlx::query!(
            r#"SELECT (SELECT count(*) FROM passkey WHERE account_id = $1) AS passkeys,
                      EXISTS (SELECT 1 FROM password_factor WHERE account_id = $1) AS "password!",
                      EXISTS (SELECT 1 FROM totp_factor WHERE account_id = $1) AS "totp!",
                      (SELECT count(*) FROM recovery_code
                        WHERE account_id = $1 AND used_at IS NULL) AS recovery_codes"#,
            account
        )
        .fetch_one(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(Factors {
            passkeys: count(r.passkeys),
            password: r.password,
            totp: r.totp,
            recovery_codes: count(r.recovery_codes),
        })
    }

    async fn passkey(&self, credential_id: &[u8]) -> StoreResult<Option<Passkey>> {
        let row = sqlx::query_as!(
            PasskeyRow,
            "SELECT credential_id, account_id, public_key, algorithm, sign_count, aaguid,
                    backup_eligible, label, created_at, last_used_at
               FROM passkey WHERE credential_id = $1",
            credential_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(Passkey::try_from).transpose()
    }

    async fn passkeys(&self, account: Uuid) -> StoreResult<Vec<Passkey>> {
        let rows = sqlx::query_as!(
            PasskeyRow,
            "SELECT credential_id, account_id, public_key, algorithm, sign_count, aaguid,
                    backup_eligible, label, created_at, last_used_at
               FROM passkey WHERE account_id = $1 ORDER BY created_at, credential_id",
            account
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(Passkey::try_from).collect()
    }

    async fn password(&self, account: Uuid) -> StoreResult<Option<String>> {
        sqlx::query_scalar!("SELECT phc FROM password_factor WHERE account_id = $1", account)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)
    }

    async fn totp(&self, account: Uuid) -> StoreResult<Option<TotpFactor>> {
        let row = sqlx::query!(
            "SELECT sealed_by, sealed, last_step FROM totp_factor WHERE account_id = $1",
            account
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            let last_step = r
                .last_step
                .map(u64::try_from)
                .transpose()
                .map_err(|_| StoreError::Corrupt(format!("totp of {account}: last step")))?;
            Ok(TotpFactor {
                sealed: SealedKey { sealed_by: r.sealed_by, sealed: r.sealed },
                last_step,
            })
        })
        .transpose()
    }
}

impl AccountWrite for PgTx {
    async fn insert_account(&mut self, a: &Account) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO account (id, display_name, name, created_by, created_at, disabled_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
            a.id,
            a.display_name,
            a.name,
            a.created_by,
            from_time(a.created_at),
            a.disabled_at.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn name_account(&mut self, account: Uuid, name: &str) -> StoreResult<()> {
        sqlx::query!("UPDATE account SET name = $2 WHERE id = $1", account, name)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn insert_invitation(&mut self, i: &Invitation, digest: &[u8; 32]) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO invitation (id, account_id, secret_hash, issued_by, expires_at,
                                     passkey_only, redeemed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            i.id,
            i.account,
            digest.as_slice(),
            i.issued_by,
            from_time(i.expires_at),
            i.passkey_only,
            i.redeemed_at.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn redeem_invitation(&mut self, id: Uuid, now: SystemTime) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE invitation SET redeemed_at = $2
              WHERE id = $1 AND redeemed_at IS NULL AND expires_at > $2",
            id,
            from_time(now)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn insert_passkey(&mut self, p: &Passkey) -> StoreResult<()> {
        factor::insert_passkey(self, p).await
    }

    async fn passkey_used(
        &mut self,
        credential_id: &[u8],
        seen: u32,
        count: u32,
        at: SystemTime,
    ) -> StoreResult<bool> {
        factor::passkey_used(self, credential_id, seen, count, at).await
    }

    async fn remove_passkey(&mut self, account: Uuid, credential_id: &[u8]) -> StoreResult<bool> {
        factor::remove_passkey(self, account, credential_id).await
    }

    async fn set_password(&mut self, account: Uuid, phc: &str, at: SystemTime) -> StoreResult<()> {
        factor::set_password(self, account, phc, at).await
    }

    async fn remove_password(&mut self, account: Uuid) -> StoreResult<bool> {
        factor::remove_password(self, account).await
    }

    async fn set_totp(&mut self, account: Uuid, sealed: &SealedKey, step: u64) -> StoreResult<()> {
        factor::set_totp(self, account, sealed, step).await
    }

    async fn totp_used(&mut self, account: Uuid, step: u64) -> StoreResult<bool> {
        factor::totp_used(self, account, step).await
    }

    async fn remove_totp(&mut self, account: Uuid) -> StoreResult<bool> {
        factor::remove_totp(self, account).await
    }

    async fn replace_recovery_codes(
        &mut self,
        account: Uuid,
        digests: &[[u8; 32]],
    ) -> StoreResult<()> {
        factor::replace_recovery_codes(self, account, digests).await
    }

    async fn use_recovery_code(
        &mut self,
        account: Uuid,
        digest: &[u8; 32],
        at: SystemTime,
    ) -> StoreResult<bool> {
        factor::use_recovery_code(self, account, digest, at).await
    }

    async fn insert_challenge(&mut self, c: &Challenge, now: SystemTime) -> StoreResult<()> {
        challenge::insert(self, c, now).await
    }

    async fn take_challenge(
        &mut self,
        id: Uuid,
        now: SystemTime,
    ) -> StoreResult<Option<Challenge>> {
        challenge::take(self, id, now).await
    }
}
