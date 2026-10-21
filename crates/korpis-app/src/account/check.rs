// SPDX-License-Identifier: AGPL-3.0-or-later

//! Checking a factor (§9.6 of docs/08-identity.md), for a sign-in and for a re-authentication
//! alike: a passkey's assertion, or a password with a code beside it. Every refusal is
//! [`AppError::Unauthenticated`], whichever part failed, and a password is checked against a
//! stand-in hash when there is no account to check it against, so how long a refusal takes does
//! not say whether a sign-in name exists.

use super::Client;
use super::record;
use super::sign_in::{Assertion, SecondFactor};
use crate::App;
use crate::error::AppError;
use crate::ports::{AccountWrite as _, Clock, RecipeSource, Store, Tx as _};
use korpis_core::account::password::{STAND_IN, outdated};
use korpis_core::account::record::{Account, Ceremony, sign_in_name};
use korpis_core::account::recovery;
use korpis_core::principal::Principal;
use korpis_core::webauthn::{StoredPasskey, WebAuthnError, verify_assertion};
use std::future::Future;
use uuid::Uuid;

/// A passkey assertion that verified, not yet spent.
pub(super) struct CheckedPasskey {
    /// The account the passkey belongs to.
    pub account: Uuid,
    credential_id: Vec<u8>,
    sign_count: u32,
    count: u32,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Verifies the passkey assertion `a`, answered at the address `portal` names, where the
    /// passkey must be registered (§9.8 of docs/08-identity.md); spent with
    /// [`Self::spend_passkey`].
    pub(super) async fn check_passkey(
        &self,
        a: Assertion,
        portal: Option<Uuid>,
    ) -> Result<CheckedPasskey, AppError> {
        let challenge = self.take_challenge(a.challenge, Ceremony::SignIn, portal).await?;
        let passkey = self.store.passkey(&a.credential_id).await?;
        let passkey = passkey.filter(|p| p.portal == portal).ok_or(AppError::Unauthenticated)?;
        self.usable(passkey.account).await?;
        let rp = self.relying_party_at(portal).await?;
        let stored =
            StoredPasskey { public_key: &passkey.public_key, sign_count: passkey.sign_count };
        let verified = verify_assertion(
            &rp,
            &challenge.challenge,
            &stored,
            &a.client_data_json,
            &a.authenticator_data,
            &a.signature,
        );
        let count = match verified {
            Ok(count) => count,
            Err(WebAuthnError::Counter) => {
                self.suspect(passkey.account, &passkey.label).await?;
                return Err(AppError::Unauthenticated);
            }
            Err(_) => return Err(AppError::Unauthenticated),
        };
        Ok(CheckedPasskey {
            account: passkey.account,
            credential_id: passkey.credential_id,
            sign_count: passkey.sign_count,
            count,
        })
    }

    /// Advances the counter of the passkey `checked` verified, in `tx`; refused if another
    /// assertion advanced it first.
    pub(super) async fn spend_passkey(
        &self,
        tx: &mut S::Tx,
        checked: &CheckedPasskey,
    ) -> Result<(), AppError> {
        let (id, now) = (&checked.credential_id, self.clock.now());
        if tx.passkey_used(id, checked.sign_count, checked.count, now).await? {
            Ok(())
        } else {
            Err(AppError::Unauthenticated)
        }
    }

    /// Records that a passkey of `account` answered with a counter that did not advance: two
    /// authenticators hold its credential, and its holder should know (§9.6).
    async fn suspect(&self, account: Uuid, label: &str) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        let more = [("passkey", label.into())];
        record(&mut tx, &Principal::User(account), "account.passkey_cloned", account, more).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Account `id`, if it may sign in.
    async fn usable(&self, id: Uuid) -> Result<Account, AppError> {
        self.store
            .account(id)
            .await?
            .filter(|a| a.disabled_at.is_none())
            .ok_or(AppError::Unauthenticated)
    }

    /// Runs `check`, a guess at `name`'s password from `client`, inside the buckets that bound
    /// guessing.
    ///
    /// A password and a six-digit code can be guessed, as no other credential can, so the
    /// buckets are charged before anything is checked: the failures of the source, and the
    /// guesses at the name, or at the device and its account when the browser holds a device
    /// cookie for it. Once one refuses, nothing is checked, the right password included, so a
    /// guesser that ignores the refusal learns nothing more (Finding 69 of
    /// docs/23-walkthroughs.md). They are charged in one statement that admits or refuses, and
    /// given back when the check succeeds, so guesses sent at once cannot all pass a bucket that
    /// has room for one (Findings 70 and 71); a refusal is then recorded as a failed attempt and
    /// charged nothing more.
    pub(super) async fn guarded<T>(
        &self,
        name: &str,
        client: &Client,
        check: impl Future<Output = Result<T, AppError>>,
    ) -> Result<T, AppError> {
        let buckets = self.charge_guess(client.ip, name, &client.devices).await?;
        let checked = check.await;
        match &checked {
            Ok(_) => {
                for bucket in &buckets {
                    self.store.refund(bucket, self.clock.now()).await?;
                }
            }
            Err(AppError::Unauthenticated) => self.guess_failed(client.ip).await?,
            Err(_) => {}
        }
        checked
    }

    /// Checks `password` and `second` for `name`, answering the account, a transaction in which
    /// the second factor is spent, and the factors checked; the caller records what they were
    /// for and commits. Everything read is read before the transaction begins, so it never holds
    /// a connection while asking the pool for another (Finding 71).
    pub(super) async fn check_password(
        &self,
        name: &str,
        password: &str,
        second: SecondFactor,
    ) -> Result<(Uuid, S::Tx, &'static str), AppError> {
        let Some((account, phc)) = self.password_of(name).await? else {
            let _ = self.passwords().matches(password, STAND_IN).await;
            return Err(AppError::Unauthenticated);
        };
        if !self.passwords().matches(password, &phc).await {
            return Err(AppError::Unauthenticated);
        }
        let totp = match &second {
            SecondFactor::Totp(_) => self.store.totp(account).await?,
            SecondFactor::Recovery(_) => None,
        };
        let rehashed =
            if outdated(&phc) { self.passwords().hash(password).await.ok() } else { None };
        let mut tx = self.store.begin().await?;
        let now = self.clock.now();
        let factor = match &second {
            SecondFactor::Totp(code) => {
                let totp = totp.ok_or(AppError::Unauthenticated)?;
                let secret = self
                    .factor_vault()?
                    .open(account, &totp.sealed)
                    .map_err(|e| AppError::Internal(e.to_string()))?;
                let step =
                    secret.accept(code, now, totp.last_step).ok_or(AppError::Unauthenticated)?;
                if !tx.totp_used(account, step).await? {
                    return Err(AppError::Unauthenticated);
                }
                "password+totp"
            }
            SecondFactor::Recovery(code) => {
                if !tx.use_recovery_code(account, &recovery::digest(code), now).await? {
                    return Err(AppError::Unauthenticated);
                }
                "password+recovery_code"
            }
        };
        if let Some(fresh) = rehashed {
            tx.set_password(account, &fresh, now).await?;
        }
        Ok((account, tx, factor))
    }

    /// The account `name` names and its password hash, if it may sign in with one.
    async fn password_of(&self, name: &str) -> Result<Option<(Uuid, String)>, AppError> {
        let Ok(name) = sign_in_name(name) else { return Ok(None) };
        let Some(account) = self.store.account_named(&name).await? else { return Ok(None) };
        if account.disabled_at.is_some() {
            return Ok(None);
        }
        Ok(self.store.password(account.id).await?.map(|phc| (account.id, phc)))
    }
}
