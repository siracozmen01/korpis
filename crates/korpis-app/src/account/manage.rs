// SPDX-License-Identifier: AGPL-3.0-or-later

//! A signed-in user managing their own factors (§9.3 and §9.6 of docs/08-identity.md), each
//! change behind a recent sign-in (`recent`). Two rules hold after every change: something can
//! still sign in, a passkey or a password; and a password has a lasting second factor beside it,
//! a passkey or TOTP, since recovery codes run out. Each change locks the account first, so two
//! removals racing cannot leave it with neither. Changing the password or TOTP, the secrets a
//! guess is aimed at, stops every device cookie issued before it being honoured (`device`).

use super::reach::{registers_passkeys, sees_passkey, still_changeable};
use super::{Registration, fresh_codes, own_account, record};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{AccountWrite as _, Clock, RecipeSource, Store, Tx};
use korpis_core::account::exposed::Shared;
use korpis_core::account::record::{Account, Ceremony, Factors, Passkey};
use korpis_core::account::totp::TotpSecret;
use korpis_core::webauthn::verify_registration;
use uuid::Uuid;

/// An account as its holder sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountView {
    /// The account.
    pub account: Account,
    /// What it holds.
    pub factors: Factors,
    /// Its passkeys.
    pub passkeys: Vec<Passkey>,
}

/// Whether `f` keeps both rules.
const fn sound(f: &Factors) -> bool {
    f.any() && (!f.password || f.passkeys > 0 || f.totp)
}

fn unsound() -> AppError {
    AppError::conflict(
        "account.last_factor",
        "the account would be left without a way to sign in, or a password without a passkey or \
         TOTP beside it",
    )
}

/// Locks `account` in `tx` and answers its factors.
async fn locked<T: Tx>(tx: &mut T, account: Uuid) -> Result<Factors, AppError> {
    tx.lock_factors(account).await?.ok_or(AppError::NotFound { kind: "account" })
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The caller's account, with the passkeys it sees: for a confined session, those of its
    /// own portal (§9.8 of docs/08-identity.md).
    pub async fn own_account(&self, caller: &Caller) -> Result<AccountView, AppError> {
        let id = own_account(caller)?;
        let own = self.confining(caller).await?;
        let account =
            self.store.account(id).await?.ok_or(AppError::NotFound { kind: "account" })?;
        let factors = self.store.factors(id).await?;
        let mut passkeys = self.store.passkeys(id).await?;
        passkeys.retain(|p| sees_passkey(own.as_ref(), p.portal));
        Ok(AccountView { account, factors, passkeys })
    }

    /// Begins registering another passkey for the caller, at the address its session began
    /// at: a session begun at a portal registers passkeys there alone, and only a session
    /// confined exactly as its address is registers any (§9.8 of docs/08-identity.md).
    pub async fn begin_add_passkey(&self, caller: &Caller) -> Result<Registration, AppError> {
        let (_, session) = self.recently_signed_in_session(caller).await?;
        registers_passkeys(&session)?;
        let at = session.portal.map(|p| p.portal);
        let view = self.own_account(caller).await?;
        let name = view.account.name.clone().unwrap_or_else(|| view.account.id.to_string());
        let exclude =
            view.passkeys.into_iter().filter(|p| p.portal == at).map(|p| p.credential_id).collect();
        self.registration((&view.account, &name), None, exclude, at).await
    }

    /// Adds the passkey `r` registers, answering a challenge [`Self::begin_add_passkey`] issued.
    pub async fn add_passkey(
        &self,
        caller: &Caller,
        r: super::NewRegistration,
    ) -> Result<(), AppError> {
        let (id, session) = self.recently_signed_in_session(caller).await?;
        registers_passkeys(&session)?;
        let at = session.portal.map(|p| p.portal);
        let challenge = self.take_challenge(r.challenge, Ceremony::Register, at).await?;
        if challenge.account != Some(id) || challenge.invitation.is_some() {
            return Err(AppError::Unauthenticated);
        }
        let new = verify_registration(
            &self.relying_party_at(at).await?,
            &challenge.challenge,
            &r.client_data_json,
            &r.attestation_object,
        )
        .map_err(|e| AppError::invalid("registration", e.to_string()))?;
        let passkey = Passkey::registered(new, id, super::label(&r.label)?, self.clock.now(), at);
        let mut tx = self.store.begin().await?;
        locked(&mut tx, id).await?;
        tx.insert_passkey(&passkey).await.map_err(super::redeem::registered)?;
        record(
            &mut tx,
            &caller.principal,
            "account.factor_added",
            id,
            [("factor", "passkey".into())],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Removes the caller's passkey `credential_id`.
    pub async fn remove_passkey(
        &self,
        caller: &Caller,
        credential_id: &[u8],
    ) -> Result<(), AppError> {
        let id = self.recently_signed_in(caller).await?;
        let own = self.confining(caller).await?;
        let seen = self.store.passkey(credential_id).await?;
        if !seen.is_some_and(|p| p.account == id && sees_passkey(own.as_ref(), p.portal)) {
            return Err(AppError::NotFound { kind: "passkey" });
        }
        let mut tx = self.store.begin().await?;
        let mut after = locked(&mut tx, id).await?;
        if !tx.remove_passkey(id, credential_id).await? {
            return Err(AppError::NotFound { kind: "passkey" });
        }
        after.passkeys = after.passkeys.saturating_sub(1);
        if !sound(&after) {
            return Err(unsound());
        }
        record(
            &mut tx,
            &caller.principal,
            "account.factor_removed",
            id,
            [("factor", "passkey".into())],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Sets the caller's password, which needs a passkey or TOTP beside it.
    pub async fn set_password(&self, caller: &Caller, password: &str) -> Result<(), AppError> {
        let (id, session) = self.recently_signed_in_session(caller).await?;
        let checked = self.secrets_changeable(id, session.within).await?;
        let phc = self
            .passwords()
            .hash(password)
            .await
            .map_err(|e| AppError::invalid("password", e.to_string()))?;
        let mut tx = self.store.begin().await?;
        let mut after = locked(&mut tx, id).await?;
        still_changeable(&mut tx, id, checked.as_ref()).await?;
        after.password = true;
        if !sound(&after) {
            return Err(unsound());
        }
        let now = self.clock.now();
        tx.set_password(id, &phc, now).await?;
        tx.secret_set_within(id, Shared::Password, session.within).await?;
        tx.secrets_changed(id, now).await?;
        record(
            &mut tx,
            &caller.principal,
            "account.factor_added",
            id,
            [("factor", "password".into())],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Removes the caller's password.
    pub async fn remove_password(&self, caller: &Caller) -> Result<(), AppError> {
        let (id, session) = self.recently_signed_in_session(caller).await?;
        let checked = self.secrets_changeable(id, session.within).await?;
        let mut tx = self.store.begin().await?;
        let mut after = locked(&mut tx, id).await?;
        still_changeable(&mut tx, id, checked.as_ref()).await?;
        if !tx.remove_password(id).await? {
            return Err(AppError::NotFound { kind: "password" });
        }
        tx.secret_set_within(id, Shared::Password, None).await?;
        tx.secrets_changed(id, self.clock.now()).await?;
        after.password = false;
        if !sound(&after) {
            return Err(unsound());
        }
        record(
            &mut tx,
            &caller.principal,
            "account.factor_removed",
            id,
            [("factor", "password".into())],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Enrols `secret` as the caller's TOTP factor, replacing any, once `code` shows the
    /// authenticator app reads it.
    pub async fn enrol_totp(
        &self,
        caller: &Caller,
        secret: &TotpSecret,
        code: &str,
    ) -> Result<(), AppError> {
        let (id, session) = self.recently_signed_in_session(caller).await?;
        let checked = self.secrets_changeable(id, session.within).await?;
        let step = secret
            .accept(code, self.clock.now(), None)
            .ok_or_else(|| AppError::invalid("code", "not the code the secret gives now"))?;
        let sealed =
            self.factor_vault()?.seal(id, secret).map_err(|e| AppError::Internal(e.to_string()))?;
        let mut tx = self.store.begin().await?;
        locked(&mut tx, id).await?;
        still_changeable(&mut tx, id, checked.as_ref()).await?;
        tx.set_totp(id, &sealed, step).await?;
        tx.secret_set_within(id, Shared::Totp, session.within).await?;
        tx.secrets_changed(id, self.clock.now()).await?;
        record(&mut tx, &caller.principal, "account.factor_added", id, [("factor", "totp".into())])
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Removes the caller's TOTP factor.
    pub async fn remove_totp(&self, caller: &Caller) -> Result<(), AppError> {
        let (id, session) = self.recently_signed_in_session(caller).await?;
        let checked = self.secrets_changeable(id, session.within).await?;
        let mut tx = self.store.begin().await?;
        let mut after = locked(&mut tx, id).await?;
        still_changeable(&mut tx, id, checked.as_ref()).await?;
        if !tx.remove_totp(id).await? {
            return Err(AppError::NotFound { kind: "totp" });
        }
        tx.secret_set_within(id, Shared::Totp, None).await?;
        tx.secrets_changed(id, self.clock.now()).await?;
        after.totp = false;
        if !sound(&after) {
            return Err(unsound());
        }
        record(
            &mut tx,
            &caller.principal,
            "account.factor_removed",
            id,
            [("factor", "totp".into())],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Replaces the caller's recovery codes with ten fresh ones, answered once.
    pub async fn renew_recovery_codes(&self, caller: &Caller) -> Result<Vec<String>, AppError> {
        let (id, session) = self.recently_signed_in_session(caller).await?;
        let checked = self.secrets_changeable(id, session.within).await?;
        let mut tx = self.store.begin().await?;
        locked(&mut tx, id).await?;
        still_changeable(&mut tx, id, checked.as_ref()).await?;
        let codes = fresh_codes(&mut tx, id).await?;
        tx.secret_set_within(id, Shared::RecoveryCodes, session.within).await?;
        record(&mut tx, &caller.principal, "account.recovery_codes_renewed", id, []).await?;
        tx.commit().await?;
        Ok(codes)
    }
}
