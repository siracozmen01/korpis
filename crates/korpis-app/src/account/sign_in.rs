// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in (§9.6 of docs/08-identity.md): a passkey alone, or a password with a TOTP code or a
//! recovery code beside it, each checked as `check` says, and a session begun.

use super::check::Checked;
use super::{Begun, Client, record};
use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store, Tx as _};
use korpis_core::principal::Principal;
use korpis_core::rate::Standing;
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
        let checked = self.check_passkey(a, client.address()).await?;
        let mut tx = self.store.begin().await?;
        self.spend_passkey(&mut tx, &checked).await?;
        self.signed_in(tx, checked.account, ("passkey", None), client).await
    }

    /// Signs in as `name` with `password` and `second` beside it, from `client`, inside the
    /// buckets that bound guessing (`guarded`).
    pub async fn sign_in_with_password(
        &self,
        name: &str,
        password: &str,
        second: SecondFactor,
        client: &Client,
    ) -> Result<SignedIn, AppError> {
        self.guarded(name, client, async {
            let (account, tx, checked) = self.check_password(name, password, second).await?;
            self.signed_in(tx, account, checked, client).await
        })
        .await
    }

    /// Records that `account` signed in with `factor`, whose secrets were seen beneath `seen` if
    /// they were, and begins a session for `client`, in `tx`, and commits.
    async fn signed_in(
        &self,
        mut tx: S::Tx,
        account: Uuid,
        (factor, seen): Checked,
        client: &Client,
    ) -> Result<SignedIn, AppError> {
        let more = [("factor", factor.into())];
        record(&mut tx, &Principal::User(account), "account.signed_in", account, more).await?;
        let session = self.begin_session(&mut tx, account, client, seen).await?;
        tx.commit().await?;
        Ok(SignedIn { account, mfa: true, session })
    }
}

/// `RateLimited` if any of `standing` refused.
pub(super) fn refused(standing: Vec<Standing>) -> Result<(), AppError> {
    match standing.iter().filter(|s| !s.admitted).map(Standing::retry_after).max() {
        Some(retry_after) => Err(AppError::RateLimited { retry_after, standing }),
        None => Ok(()),
    }
}
