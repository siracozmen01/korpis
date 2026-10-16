// SPDX-License-Identifier: AGPL-3.0-or-later

//! Accounts, their invitations and factors, and the `WebAuthn` challenges their ceremonies answer
//! (§9.6 of docs/08-identity.md); and the vault that seals TOTP secrets.

use super::{SealedKey, StoreResult, VaultError};
use korpis_core::account::record::{Account, Challenge, Factors, Invitation, Passkey};
use korpis_core::account::totp::TotpSecret;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// A TOTP factor as kept: its secret sealed, and the step of the code accepted last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpFactor {
    /// The secret, sealed to the account.
    pub sealed: SealedKey,
    /// The step of the code accepted last.
    pub last_step: Option<u64>,
}

/// Reading accounts and their factors.
pub trait AccountRead {
    /// The account `id`.
    fn account(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Account>>> + Send;

    /// The account whose sign-in name is `name`, as [`sign_in_name`] made it.
    ///
    /// [`sign_in_name`]: korpis_core::account::record::sign_in_name
    fn account_named(
        &self,
        name: &str,
    ) -> impl Future<Output = StoreResult<Option<Account>>> + Send;

    /// The invitation whose secret has `digest`.
    fn invitation_by_digest(
        &self,
        digest: &[u8; 32],
    ) -> impl Future<Output = StoreResult<Option<Invitation>>> + Send;

    /// What factors `account` holds.
    fn factors(&self, account: Uuid) -> impl Future<Output = StoreResult<Factors>> + Send;

    /// The passkey `credential_id`.
    fn passkey(
        &self,
        credential_id: &[u8],
    ) -> impl Future<Output = StoreResult<Option<Passkey>>> + Send;

    /// The passkeys of `account`, oldest first.
    fn passkeys(&self, account: Uuid) -> impl Future<Output = StoreResult<Vec<Passkey>>> + Send;

    /// The password hash of `account`, a PHC string.
    fn password(&self, account: Uuid) -> impl Future<Output = StoreResult<Option<String>>> + Send;

    /// The TOTP factor of `account`.
    fn totp(&self, account: Uuid) -> impl Future<Output = StoreResult<Option<TotpFactor>>> + Send;
}

/// Writing accounts and their factors. Every check-and-set here answers false when what it
/// checks no longer holds, so two requests racing for one invitation, code, or counter cannot
/// both win.
pub trait AccountWrite {
    /// Adds an account.
    fn insert_account(&mut self, account: &Account)
    -> impl Future<Output = StoreResult<()>> + Send;

    /// Gives `account` the sign-in name `name`; a duplicate if another account has it.
    fn name_account(
        &mut self,
        account: Uuid,
        name: &str,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Adds an invitation, kept as the digest of its secret.
    fn insert_invitation(
        &mut self,
        invitation: &Invitation,
        digest: &[u8; 32],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Marks invitation `id` redeemed at `now`; false if it was redeemed or had expired.
    fn redeem_invitation(
        &mut self,
        id: Uuid,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Adds a passkey; a duplicate if its credential is registered already.
    fn insert_passkey(&mut self, passkey: &Passkey)
    -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that passkey `credential_id` signed in at `at` with counter `count`, if its
    /// counter is still `seen`.
    fn passkey_used(
        &mut self,
        credential_id: &[u8],
        seen: u32,
        count: u32,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Removes passkey `credential_id` of `account`; false if it has none such.
    fn remove_passkey(
        &mut self,
        account: Uuid,
        credential_id: &[u8],
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Sets the password hash of `account`, a PHC string.
    fn set_password(
        &mut self,
        account: Uuid,
        phc: &str,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Removes the password of `account`; false if it had none.
    fn remove_password(&mut self, account: Uuid) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Sets the TOTP secret of `account`, sealed, with `step` as the step of the code accepted
    /// last, the one that confirmed it.
    fn set_totp(
        &mut self,
        account: Uuid,
        sealed: &SealedKey,
        step: u64,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records the code of `step` as accepted for `account`, if no code of it or a later step
    /// was.
    fn totp_used(
        &mut self,
        account: Uuid,
        step: u64,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Removes the TOTP factor of `account`; false if it had none.
    fn remove_totp(&mut self, account: Uuid) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Replaces every recovery code of `account` with those of `digests`.
    fn replace_recovery_codes(
        &mut self,
        account: Uuid,
        digests: &[[u8; 32]],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Spends the unused recovery code of `account` with `digest`; false if there is none.
    fn use_recovery_code(
        &mut self,
        account: Uuid,
        digest: &[u8; 32],
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Adds a challenge, forgetting some that expired before `now`.
    fn insert_challenge(
        &mut self,
        challenge: &Challenge,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Takes challenge `id` if it has not expired at `now`: it answers one ceremony.
    fn take_challenge(
        &mut self,
        id: Uuid,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<Challenge>>> + Send;
}

/// What seals TOTP secrets: the control plane's factor key, in its data directory. Each seal is
/// bound to its account, so a sealed secret copied to another account does not open.
pub trait FactorVault: Send + Sync + std::fmt::Debug {
    /// `secret`, sealed to `account`.
    fn seal(&self, account: Uuid, secret: &TotpSecret) -> Result<SealedKey, VaultError>;

    /// The secret of `account`, opened from `sealed`.
    fn open(&self, account: Uuid, sealed: &SealedKey) -> Result<TotpSecret, VaultError>;
}
