// SPDX-License-Identifier: AGPL-3.0-or-later

//! Accounts (§9.6 of docs/08-identity.md): redeeming an invitation by registering a first factor,
//! signing in, and managing one's own factors. An account holds no authority by being one; what
//! it may do is the grants issued to it, so nothing here consults a grant, and every use case
//! answers a refusal of a credential as [`AppError::Unauthenticated`], saying nothing of which
//! part of it failed.

mod ceremony;
mod check;
mod completion;
mod device;
mod device_grant;
mod external;
mod manage;
mod recent;
mod redeem;
mod session;
mod sign_in;

pub use ceremony::{NewRegistration, Registration, SignInOptions};
pub use completion::{Completed, Completion};
pub use device::Device;
pub use device_grant::{Authorized, DeviceSignedIn};
pub use external::SignInBegun;
pub use manage::AccountView;
pub use redeem::{InvitationView, PasswordRedemption, Redeemed};
pub use session::{Begun, Client, Exchanged, SessionView};
pub use sign_in::{Assertion, SecondFactor, SignedIn};

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, FactorVault, Passwords, RecipeSource, Store, Tx};
use korpis_core::account::record::passkey_label;
use korpis_core::account::recovery;
use korpis_core::audit::{Effect, Outcome};
use korpis_core::principal::{Holder, Principal};
use korpis_core::webauthn::RelyingParty;
use serde_json::{Map, Value};
use std::sync::Arc;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The relying party passkeys are registered with, or why there is none.
    pub(crate) fn relying_party(&self) -> Result<&RelyingParty, AppError> {
        self.relying_party.as_ref().ok_or_else(|| AppError::Unavailable {
            what: "passkeys",
            detail: "no relying party is configured".into(),
        })
    }

    /// The vault TOTP secrets are sealed by, or why there is none.
    pub(crate) fn factor_vault(&self) -> Result<&Arc<dyn FactorVault>, AppError> {
        self.factor_vault.as_ref().ok_or_else(|| AppError::Unavailable {
            what: "TOTP",
            detail: "no factor vault is configured".into(),
        })
    }

    /// What hashes and checks passwords.
    pub(crate) fn passwords(&self) -> &dyn Passwords {
        self.passwords.as_ref()
    }
}

/// The account a caller signed in as, or a refusal: factors are managed by their holder alone.
pub(crate) fn own_account(caller: &Caller) -> Result<Uuid, AppError> {
    match caller.principal {
        Principal::User(id) => Ok(id),
        _ => Err(AppError::invalid("caller", "only a signed-in user has factors")),
    }
}

/// Fresh recovery codes for `account`, replacing any it had: answered once, kept as digests.
pub(crate) async fn fresh_codes<T: Tx>(tx: &mut T, account: Uuid) -> Result<Vec<String>, AppError> {
    let codes = recovery::generate().map_err(|e| AppError::Internal(e.to_string()))?;
    let digests: Vec<[u8; 32]> = codes.iter().map(|c| recovery::digest(c)).collect();
    tx.replace_recovery_codes(account, &digests).await?;
    Ok(codes)
}

/// Who holds the caller's sessions: the account it is, or the person whose session it calls from.
pub(crate) fn own_holder(caller: &Caller) -> Result<Holder, AppError> {
    match (caller.holder, &caller.principal) {
        (Some(holder), _) => Ok(holder),
        (None, Principal::User(id)) => Ok(Holder::Account(*id)),
        _ => Err(AppError::invalid("caller", "only a signed-in caller has sessions")),
    }
}

/// Records `action` on a session `holder` holds, as `actor`, naming the account or the binding.
pub(crate) async fn record_held<T: Tx>(
    tx: &mut T,
    actor: &Principal,
    action: &str,
    holder: Holder,
    more: impl IntoIterator<Item = (&'static str, Value)>,
) -> Result<(), AppError> {
    let (field, id) = match holder {
        Holder::Account(id) => ("account", id),
        Holder::Person(id) => ("binding", id),
    };
    let mut fields = Map::new();
    fields.insert(field.into(), Value::from(id.to_string()));
    fields.extend(more.into_iter().map(|(k, v)| (k.to_owned(), v)));
    let effect = Effect::new(actor.clone(), action, Outcome::Succeeded).after(fields);
    tx.append_effect(&effect).await?;
    Ok(())
}

/// Records `action` by `actor` on `account`, with `more` details beside the account's.
pub(crate) async fn record<T: Tx>(
    tx: &mut T,
    actor: &Principal,
    action: &str,
    account: Uuid,
    more: impl IntoIterator<Item = (&'static str, Value)>,
) -> Result<(), AppError> {
    let mut fields = Map::new();
    fields.insert("account".into(), Value::from(account.to_string()));
    fields.extend(more.into_iter().map(|(k, v)| (k.to_owned(), v)));
    let effect = Effect::new(actor.clone(), action, Outcome::Succeeded).after(fields);
    tx.append_effect(&effect).await?;
    Ok(())
}

/// `label`, checked as a passkey's label.
pub(crate) fn label(label: &str) -> Result<String, AppError> {
    passkey_label(label).map_err(|e| AppError::invalid("label", e.to_string()))
}
