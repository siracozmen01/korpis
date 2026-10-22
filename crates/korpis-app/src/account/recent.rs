// SPDX-License-Identifier: AGPL-3.0-or-later

//! A recent sign-in (§9.6 of docs/08-identity.md): what changing an account's factors needs
//! besides the session that asks. A session's sign-in, or its last re-authentication, must be
//! within ten minutes, so a session left open on a borrowed machine, or an access token copied
//! from one, cannot replace the factors and keep the account after the session ends. A session
//! re-authenticates with a factor of its own account: a passkey, or a password with a code
//! beside it, charged and limited as a sign-in is.

use super::sign_in::{Assertion, SecondFactor};
use super::{AccountView, Client, own_account, own_holder, record};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store, Tx as _, UserSessionWrite as _};
use korpis_core::account::session::{REAUTHENTICATE_WITHIN, UserSession};
use korpis_core::authz::binding::IdentityBinding;
use korpis_core::principal::Holder;
use korpis_core::principal::Principal;
use std::time::SystemTime;
use uuid::Uuid;

/// Whom a profile is of.
#[derive(Debug)]
pub enum Profiled {
    /// An account, with its factors and passkeys.
    Account(AccountView),
    /// A person signed in through an identity provider, by their own binding: they have no
    /// factors here (§9.7 of docs/08-identity.md).
    Person(Box<IdentityBinding>),
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Whom the caller is, an account or a person signed in through an identity provider, and
    /// when its session last signed in or re-authenticated; none for a caller without a session.
    pub async fn profile(
        &self,
        caller: &Caller,
    ) -> Result<(Profiled, Option<SystemTime>), AppError> {
        let holder = own_holder(caller)?;
        let profiled = match holder {
            Holder::Account(_) => Profiled::Account(self.own_account(caller).await?),
            Holder::Person(binding) => {
                let person = self.store.binding(binding).await?;
                let person = person.ok_or(AppError::NotFound { kind: "binding" })?;
                Profiled::Person(Box::new(person))
            }
        };
        let at = match caller.session {
            Some(id) => self.store.user_session(holder, id).await?.map(|s| s.authenticated_at),
            None => None,
        };
        Ok((profiled, at))
    }

    /// The caller's account, if its session signed in or re-authenticated recently; a caller
    /// without a session, one presenting an API token, never has.
    pub(super) async fn recently_signed_in(&self, caller: &Caller) -> Result<Uuid, AppError> {
        Ok(self.recently_signed_in_session(caller).await?.0)
    }

    /// The caller's account, as [`Self::recently_signed_in`] answers it, and its session: the
    /// portal it began at, the one address its passkeys are registered at, and the organization
    /// it reaches beneath, which bounds what it may change (§9.8 of docs/08-identity.md).
    pub(super) async fn recently_signed_in_session(
        &self,
        caller: &Caller,
    ) -> Result<(Uuid, UserSession), AppError> {
        let account = own_account(caller)?;
        let session = self.recent_session(caller).await?;
        match session.holder {
            Holder::Account(held) if held == account => Ok((account, session)),
            Holder::Account(_) | Holder::Person(_) => Err(AppError::Unauthenticated),
        }
    }

    /// Who holds the caller's session, an account or a person signed in through an identity
    /// provider, if it signed in or re-authenticated recently (§9.6 and §9.7 of
    /// docs/08-identity.md); a caller without a session never has.
    pub(super) async fn recently_signed_in_as(&self, caller: &Caller) -> Result<Holder, AppError> {
        Ok(self.recent_session(caller).await?.holder)
    }

    /// The caller's session, if it signed in or re-authenticated recently.
    async fn recent_session(&self, caller: &Caller) -> Result<UserSession, AppError> {
        let holder = own_holder(caller)?;
        let session = caller.session.ok_or_else(not_recent)?;
        let session =
            self.store.user_session(holder, session).await?.ok_or(AppError::Unauthenticated)?;
        let since = self.clock.now().checked_sub(REAUTHENTICATE_WITHIN);
        if since.is_some_and(|since| session.authenticated_at <= since) {
            return Err(not_recent());
        }
        Ok(session)
    }

    /// Re-authenticates the caller's session with the passkey assertion `a`.
    pub async fn reauthenticate_with_passkey(
        &self,
        caller: &Caller,
        a: Assertion,
    ) -> Result<SystemTime, AppError> {
        let (account, session) = session_of(caller)?;
        let held = self.store.user_session(Holder::Account(account), session).await?;
        let at = held.ok_or(AppError::Unauthenticated)?.portal.map(|p| p.portal);
        let checked = self.check_passkey(a, at).await?;
        if checked.account != account {
            return Err(AppError::Unauthenticated);
        }
        let mut tx = self.store.begin().await?;
        self.spend_passkey(&mut tx, &checked).await?;
        self.reauthenticated(tx, (account, session), "passkey").await
    }

    /// Re-authenticates the caller's session with its account's `password` and `second`, from
    /// `client`, charged as a password sign-in is.
    pub async fn reauthenticate_with_password(
        &self,
        caller: &Caller,
        password: &str,
        second: SecondFactor,
        client: &Client,
    ) -> Result<SystemTime, AppError> {
        let (account, session) = session_of(caller)?;
        let name = self
            .store
            .account(account)
            .await?
            .and_then(|a| a.name)
            .ok_or(AppError::Unauthenticated)?;
        let checked = self
            .guarded(&name, client, async {
                let (found, tx, (factor, _)) = self.check_password(&name, password, second).await?;
                if found != account {
                    return Err(AppError::Unauthenticated);
                }
                Ok((tx, factor))
            })
            .await?;
        let (tx, factor) = checked;
        self.reauthenticated(tx, (account, session), factor).await
    }

    /// Records that `session` of `account` re-authenticated with `factor`, in `tx`, and commits.
    async fn reauthenticated(
        &self,
        mut tx: S::Tx,
        (account, session): (Uuid, Uuid),
        factor: &'static str,
    ) -> Result<SystemTime, AppError> {
        let now = self.clock.now();
        if !tx.reauthenticated(Holder::Account(account), session, now).await? {
            return Err(AppError::Unauthenticated);
        }
        let more = [("session", session.to_string().into()), ("factor", factor.into())];
        record(&mut tx, &Principal::User(account), "session.reauthenticated", account, more)
            .await?;
        tx.commit().await?;
        Ok(now)
    }
}

/// The caller's account and session.
fn session_of(caller: &Caller) -> Result<(Uuid, Uuid), AppError> {
    let account = own_account(caller)?;
    Ok((account, caller.session.ok_or_else(not_recent)?))
}

/// The refusal of a change asked without a recent sign-in.
fn not_recent() -> AppError {
    AppError::conflict(
        "session.not_recent",
        "this needs a sign-in within ten minutes: re-authenticate first",
    )
}
