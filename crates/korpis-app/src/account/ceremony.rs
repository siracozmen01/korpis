// SPDX-License-Identifier: AGPL-3.0-or-later

//! `WebAuthn` challenges (§9.6 of docs/08-identity.md): each issued for one ceremony, kept for
//! five minutes, and taken, in a transaction of its own, by the answer that names it, so a
//! refused answer spends it as surely as an accepted one.

use crate::App;
use crate::error::AppError;
use crate::ports::{AccountWrite as _, Clock, RecipeSource, Store, Tx as _};
use korpis_core::account::nonce;
use korpis_core::account::record::{Account, CHALLENGE_LIFETIME, Ceremony, Challenge};
use uuid::Uuid;

/// What a browser needs to register a passkey: the options of `navigator.credentials.create`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The challenge's identifier, returned with the answer.
    pub challenge_id: Uuid,
    /// The bytes the authenticator signs over.
    pub challenge: [u8; 32],
    /// The relying party's identifier.
    pub rp_id: String,
    /// The user handle: the account's identifier.
    pub user_id: Uuid,
    /// The account's sign-in name.
    pub user_name: String,
    /// Its display name.
    pub display_name: String,
    /// The credentials it holds already, which the authenticator should not register again.
    pub exclude: Vec<Vec<u8>>,
}

/// A passkey registration, as the browser answered a challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRegistration {
    /// The challenge it answers.
    pub challenge: Uuid,
    /// What its holder calls the passkey.
    pub label: String,
    /// The client data, as the browser returned it.
    pub client_data_json: Vec<u8>,
    /// The attestation object, as the browser returned it.
    pub attestation_object: Vec<u8>,
}

/// What a browser needs to sign in with a passkey: the options of `navigator.credentials.get`.
/// No credential is named, so the authenticator offers the accounts it holds for the site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInOptions {
    /// The challenge's identifier, returned with the answer.
    pub challenge_id: Uuid,
    /// The bytes the authenticator signs over.
    pub challenge: [u8; 32],
    /// The relying party's identifier.
    pub rp_id: String,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Issues a challenge for `ceremony` at the address `portal` names, for `account` and
    /// `invitation` when it registers.
    async fn challenge(
        &self,
        ceremony: Ceremony,
        (account, invitation): (Option<Uuid>, Option<Uuid>),
        portal: Option<Uuid>,
    ) -> Result<Challenge, AppError> {
        let now = self.clock.now();
        let challenge = Challenge {
            id: Uuid::now_v7(),
            ceremony,
            challenge: nonce().map_err(|e| AppError::Internal(e.to_string()))?,
            account,
            invitation,
            portal,
            expires_at: now + CHALLENGE_LIFETIME,
        };
        let mut tx = self.store.begin().await?;
        tx.insert_challenge(&challenge, now).await?;
        tx.commit().await?;
        Ok(challenge)
    }

    /// A registration of a passkey for `account` as `name` at the address `portal` names,
    /// redeeming `invitation` if one is named.
    pub(super) async fn registration(
        &self,
        (account, name): (&Account, &str),
        invitation: Option<Uuid>,
        exclude: Vec<Vec<u8>>,
        portal: Option<Uuid>,
    ) -> Result<Registration, AppError> {
        let rp_id = self.relying_party_at(portal).await?.id;
        let held = (Some(account.id), invitation);
        let c = self.challenge(Ceremony::Register, held, portal).await?;
        Ok(Registration {
            challenge_id: c.id,
            challenge: c.challenge,
            rp_id,
            user_id: account.id,
            user_name: name.to_owned(),
            display_name: account.display_name.clone(),
            exclude,
        })
    }

    /// Begins a passkey sign-in at the address `portal` names, a portal or, for none, the
    /// installation's own.
    pub async fn begin_sign_in(&self, portal: Option<Uuid>) -> Result<SignInOptions, AppError> {
        let rp_id = self.relying_party_at(portal).await?.id;
        let c = self.challenge(Ceremony::SignIn, (None, None), portal).await?;
        Ok(SignInOptions { challenge_id: c.id, challenge: c.challenge, rp_id })
    }

    /// Takes challenge `id`, which must be open, for `ceremony`, and issued at the address
    /// `portal` names: an answer is verified for the relying party it was issued for, and only
    /// there.
    pub(super) async fn take_challenge(
        &self,
        id: Uuid,
        ceremony: Ceremony,
        portal: Option<Uuid>,
    ) -> Result<Challenge, AppError> {
        let mut tx = self.store.begin().await?;
        let taken = tx.take_challenge(id, self.clock.now()).await?;
        tx.commit().await?;
        taken
            .filter(|c| c.ceremony == ceremony && c.portal == portal)
            .ok_or(AppError::Unauthenticated)
    }
}
