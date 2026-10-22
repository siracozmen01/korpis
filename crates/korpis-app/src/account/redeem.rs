// SPDX-License-Identifier: AGPL-3.0-or-later

//! Redeeming an invitation (§9.6 of docs/08-identity.md): the account it was issued for gains its
//! sign-in name and first factor, a passkey or a password with TOTP beside it, and the invitation
//! is spent, in one transaction. A refused secret is [`AppError::Unauthenticated`], so nothing
//! answers whether an invitation exists.

use super::{Begun, Client, NewRegistration, Registration, fresh_codes, record};
use crate::App;
use crate::error::AppError;
use crate::ports::{AccountWrite as _, Clock, RecipeSource, Store, StoreError, Tx as _, Unique};
use korpis_core::account::exposed::Shared;
use korpis_core::account::record::{Account, Ceremony, Invitation, Passkey, sign_in_name};
use korpis_core::account::totp::TotpSecret;
use korpis_core::authz::credential::{Secret, SecretKind};
use korpis_core::principal::Principal;
use korpis_core::webauthn::verify_registration;
use std::time::SystemTime;
use uuid::Uuid;

/// What the panel shows of an invitation before it is redeemed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitationView {
    /// The account's display name, as its issuer gave it.
    pub display_name: String,
    /// When the invitation stops working.
    pub expires_at: SystemTime,
    /// Whether only registering a passkey redeems it.
    pub passkey_only: bool,
}

/// A redemption with a password, and the TOTP secret enrolled beside it with a code of it.
#[derive(Clone)]
pub struct PasswordRedemption {
    /// The sign-in name chosen.
    pub name: String,
    /// The password.
    pub password: zeroize::Zeroizing<String>,
    /// The TOTP secret the authenticator app was given.
    pub totp: TotpSecret,
    /// A code it shows now, which confirms it reads the secret.
    pub code: String,
}

impl std::fmt::Debug for PasswordRedemption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordRedemption").field("name", &self.name).finish_non_exhaustive()
    }
}

/// A redeemed invitation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redeemed {
    /// The account, now able to sign in.
    pub account: Uuid,
    /// Its recovery codes, answered once.
    pub recovery_codes: Vec<String>,
    /// The session its redemption began.
    pub session: Begun,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The invitation `secret` is, and its account, if it is open.
    async fn open_invitation(&self, secret: &str) -> Result<(Invitation, Account), AppError> {
        let secret = Secret::presented_as(SecretKind::Invitation, secret)
            .ok_or(AppError::Unauthenticated)?;
        let invitation = self
            .store
            .invitation_by_digest(&secret.digest())
            .await?
            .filter(|i| i.open(self.clock.now()))
            .ok_or(AppError::Unauthenticated)?;
        let account = self
            .store
            .account(invitation.account)
            .await?
            .filter(|a| a.disabled_at.is_none())
            .ok_or(AppError::Unauthenticated)?;
        Ok((invitation, account))
    }

    /// What the invitation `secret` is for, if it is open.
    pub async fn invitation(&self, secret: &str) -> Result<InvitationView, AppError> {
        let (invitation, account) = self.open_invitation(secret).await?;
        Ok(InvitationView {
            display_name: account.display_name,
            expires_at: invitation.expires_at,
            passkey_only: invitation.passkey_only,
        })
    }

    /// `name`, checked as a sign-in name nobody but `account` holds.
    async fn free_name(&self, account: Uuid, name: &str) -> Result<String, AppError> {
        let name = sign_in_name(name).map_err(|e| AppError::invalid("name", e.to_string()))?;
        match self.store.account_named(&name).await? {
            Some(other) if other.id != account => Err(taken()),
            _ => Ok(name),
        }
    }

    /// Begins redeeming the invitation `secret` by registering a passkey, as `name`, at the
    /// address `portal` names, whose passkey it will be (§9.8 of docs/08-identity.md).
    pub async fn begin_redemption(
        &self,
        secret: &str,
        name: &str,
        portal: Option<Uuid>,
    ) -> Result<Registration, AppError> {
        let (invitation, account) = self.open_invitation(secret).await?;
        let name = self.free_name(account.id, name).await?;
        self.registration((&account, &name), Some(invitation.id), Vec::new(), portal).await
    }

    /// Redeems the invitation `secret` as `name` with the passkey registration `r`, answering
    /// the challenge [`Self::begin_redemption`] issued, and begins a session for `client`.
    pub async fn redeem_with_passkey(
        &self,
        secret: &str,
        (name, r): (&str, NewRegistration),
        client: &Client,
    ) -> Result<Redeemed, AppError> {
        let (invitation, account) = self.open_invitation(secret).await?;
        let name = self.free_name(account.id, name).await?;
        let at = client.address();
        let challenge = self.take_challenge(r.challenge, Ceremony::Register, at).await?;
        if challenge.invitation != Some(invitation.id) || challenge.account != Some(account.id) {
            return Err(AppError::Unauthenticated);
        }
        let new = verify_registration(
            &self.relying_party_at(at).await?,
            &challenge.challenge,
            &r.client_data_json,
            &r.attestation_object,
        )
        .map_err(|_| AppError::Unauthenticated)?;
        let label = super::label(&r.label)?;
        let passkey = Passkey::registered(new, account.id, label, self.clock.now(), at);
        let mut tx = self.store.begin().await?;
        self.spend(&mut tx, &invitation, &account, &name).await?;
        tx.insert_passkey(&passkey).await.map_err(registered)?;
        self.finish_redemption(tx, (&invitation, account.id), "passkey", client).await
    }

    /// Redeems the invitation `secret` with a password and TOTP beside it, and begins a session
    /// for `client`.
    pub async fn redeem_with_password(
        &self,
        secret: &str,
        r: PasswordRedemption,
        client: &Client,
    ) -> Result<Redeemed, AppError> {
        let (invitation, account) = self.open_invitation(secret).await?;
        if invitation.passkey_only {
            return Err(AppError::invalid("invitation", "only registering a passkey redeems it"));
        }
        let name = self.free_name(account.id, &r.name).await?;
        let step = r
            .totp
            .accept(&r.code, self.clock.now(), None)
            .ok_or_else(|| AppError::invalid("code", "not the code the secret gives now"))?;
        let phc = self
            .passwords()
            .hash(&r.password)
            .await
            .map_err(|e| AppError::invalid("password", e.to_string()))?;
        let sealed = self
            .factor_vault()?
            .seal(account.id, &r.totp)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let mut tx = self.store.begin().await?;
        self.spend(&mut tx, &invitation, &account, &name).await?;
        tx.set_password(account.id, &phc, self.clock.now()).await?;
        tx.set_totp(account.id, &sealed, step).await?;
        // Set at the address the redemption reached, whose portal's holder could see them.
        let within = client.portal.map(|p| p.organization);
        tx.secret_set_within(account.id, Shared::Password, within).await?;
        tx.secret_set_within(account.id, Shared::Totp, within).await?;
        self.finish_redemption(tx, (&invitation, account.id), "password+totp", client).await
    }

    /// Spends `invitation` and names `account` `name`, in `tx`.
    async fn spend(
        &self,
        tx: &mut S::Tx,
        invitation: &Invitation,
        account: &Account,
        name: &str,
    ) -> Result<(), AppError> {
        if !tx.redeem_invitation(invitation.id, self.clock.now()).await? {
            return Err(AppError::Unauthenticated);
        }
        tx.name_account(account.id, name).await.map_err(|e| match e {
            StoreError::Duplicate(Unique::AccountName) => taken(),
            other => other.into(),
        })
    }

    /// Gives the account its recovery codes, shown at the address `client` reached, records the
    /// redemption, begins a session for `client`, and commits.
    async fn finish_redemption(
        &self,
        mut tx: S::Tx,
        (invitation, account): (&Invitation, Uuid),
        factor: &str,
        client: &Client,
    ) -> Result<Redeemed, AppError> {
        let recovery_codes = fresh_codes(&mut tx, account).await?;
        let within = client.portal.map(|p| p.organization);
        tx.secret_set_within(account, Shared::RecoveryCodes, within).await?;
        let more = [("invitation", invitation.id.to_string().into()), ("factor", factor.into())];
        record(&mut tx, &Principal::User(account), "account.redeemed", account, more).await?;
        let session = self.begin_session(&mut tx, account, client, None).await?;
        tx.commit().await?;
        Ok(Redeemed { account, recovery_codes, session })
    }
}

fn taken() -> AppError {
    AppError::conflict("account.name_taken", "another account has that sign-in name")
}

/// A refused passkey insert, as the caller should hear it.
pub(super) fn registered(e: StoreError) -> AppError {
    match e {
        StoreError::Duplicate(Unique::Passkey) => {
            AppError::conflict("passkey.registered", "that passkey is registered already")
        }
        other => other.into(),
    }
}
