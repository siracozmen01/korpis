// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in (§9.6 of docs/08-identity.md): a passkey alone, or a password with a TOTP code or a
//! recovery code beside it. Every refusal is [`AppError::Unauthenticated`], whichever part
//! failed, and a password is checked against a stand-in hash when there is no account to check it
//! against, so how long a refusal takes does not say whether a sign-in name exists.

use super::{Begun, Client, record};
use crate::App;
use crate::error::AppError;
use crate::ports::{AccountWrite as _, Clock, RecipeSource, Store, Tx as _};
use korpis_core::account::password::{STAND_IN, outdated};
use korpis_core::account::record::{Account, Ceremony, sign_in_name};
use korpis_core::account::recovery;
use korpis_core::principal::Principal;
use korpis_core::rate::{Bucket, Standing};
use korpis_core::webauthn::{StoredPasskey, WebAuthnError, verify_assertion};
use uuid::Uuid;

/// What stands beside a password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecondFactor {
    /// A TOTP code.
    Totp(String),
    /// A recovery code, which is then spent.
    Recovery(String),
}

/// A passkey assertion answering a sign-in challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    /// The challenge it answers.
    pub challenge: Uuid,
    /// The credential that signed.
    pub credential_id: Vec<u8>,
    /// The client data, as the browser returned it.
    pub client_data_json: Vec<u8>,
    /// The authenticator data, as the browser returned it.
    pub authenticator_data: Vec<u8>,
    /// The signature.
    pub signature: Vec<u8>,
}

/// A sign-in that succeeded, and the session it began.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedIn {
    /// The account.
    pub account: Uuid,
    /// Whether it satisfies `requires_mfa` (§9 of docs/08-identity.md): every sign-in this
    /// accepts does, since none is one factor.
    pub mfa: bool,
    /// The session, and its refresh credential.
    pub session: Begun,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Signs in with the passkey assertion `a`, beginning a session for `client`.
    pub async fn sign_in_with_passkey(
        &self,
        a: Assertion,
        client: &Client,
    ) -> Result<SignedIn, AppError> {
        let challenge = self.take_challenge(a.challenge, Ceremony::SignIn).await?;
        let passkey =
            self.store.passkey(&a.credential_id).await?.ok_or(AppError::Unauthenticated)?;
        self.usable(passkey.account).await?;
        let stored =
            StoredPasskey { public_key: &passkey.public_key, sign_count: passkey.sign_count };
        let verified = verify_assertion(
            self.relying_party()?,
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
        let mut tx = self.store.begin().await?;
        let now = self.clock.now();
        if !tx.passkey_used(&passkey.credential_id, passkey.sign_count, count, now).await? {
            return Err(AppError::Unauthenticated);
        }
        let more = [("factor", "passkey".into())];
        record(
            &mut tx,
            &Principal::User(passkey.account),
            "account.signed_in",
            passkey.account,
            more,
        )
        .await?;
        let session = self.begin_session(&mut tx, passkey.account, client).await?;
        tx.commit().await?;
        Ok(SignedIn { account: passkey.account, mfa: true, session })
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

    /// Signs in as `name` with `password` and `second` beside it, from `ip`.
    ///
    /// A password and a six-digit code can be guessed, as no other credential can, so the
    /// buckets that bound guessing are asked before anything is checked: the failures of the
    /// source, and the guesses at the name. Once either refuses, nothing is checked, the right
    /// password included, so a guesser that ignores the refusal learns nothing more (Finding 69
    /// of docs/23-walkthroughs.md). Both are charged before the check, in one statement that
    /// admits or refuses, and given back when the sign-in succeeds, so guesses sent at once
    /// cannot all pass a bucket that has room for one (Findings 70 and 71); a refusal is then
    /// recorded as a failed attempt and charged nothing more.
    pub async fn sign_in_with_password(
        &self,
        name: &str,
        password: &str,
        second: SecondFactor,
        client: &Client,
    ) -> Result<SignedIn, AppError> {
        let ip = client.ip;
        let rate = self.limiter.rates.guesses;
        let guessed = sign_in_name(name).ok().map(|n| Bucket {
            policy: "guesses",
            key: format!("n:{n}"),
            rate,
        });
        let buckets: Vec<Bucket> =
            std::iter::once(self.failure_bucket(ip)).chain(guessed).collect();
        refused(self.store.charge(buckets.clone(), self.clock.now()).await?)?;
        let checked = self.check_password(name, password, second, client).await;
        match &checked {
            Ok(_) => {
                for bucket in &buckets {
                    self.store.refund(bucket, self.clock.now()).await?;
                }
            }
            Err(AppError::Unauthenticated) => self.guess_failed(ip).await?,
            Err(_) => {}
        }
        checked
    }

    /// Checks `password` and `second` for `name`, and records the sign-in and begins a session
    /// for `client` if both hold. Everything read is read before the transaction begins, so it
    /// never holds a connection while asking the pool for another (Finding 71).
    async fn check_password(
        &self,
        name: &str,
        password: &str,
        second: SecondFactor,
        client: &Client,
    ) -> Result<SignedIn, AppError> {
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
        record(
            &mut tx,
            &Principal::User(account),
            "account.signed_in",
            account,
            [("factor", factor.into())],
        )
        .await?;
        let session = self.begin_session(&mut tx, account, client).await?;
        tx.commit().await?;
        Ok(SignedIn { account, mfa: true, session })
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

/// `RateLimited` if any of `standing` refused.
fn refused(standing: Vec<Standing>) -> Result<(), AppError> {
    match standing.iter().filter(|s| !s.admitted).map(Standing::retry_after).max() {
        Some(retry_after) => Err(AppError::RateLimited { retry_after, standing }),
        None => Ok(()),
    }
}
