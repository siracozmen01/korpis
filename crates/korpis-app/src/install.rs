// SPDX-License-Identifier: AGPL-3.0-or-later

//! Installation: the first organization and the first grant (§9.1 of docs/08-identity.md).
//! Every other grant descends from the one created here, held by an account whose bootstrap
//! invitation only registering a passkey redeems, or, for what cannot register one, by an API
//! token; and `recover` issues another to a new account when the last holder is locked out
//! (§9.2).

mod recover;

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store, TenancyWrite, Tx};
use korpis_core::account::record::{Account, BOOTSTRAP_LIFETIME, Invitation};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::action::CORE;
use korpis_core::authz::credential::{ApiToken, Secret, SecretKind};
use korpis_core::authz::{
    Action, Conditions, Delegation, Grant, GrantRecord, Kind, ObjectRef, Scope,
};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::tenancy::{Name, Organization, OrganizationStatus};
use std::collections::BTreeSet;
use std::time::SystemTime;
use uuid::Uuid;

/// What holds an installation's root grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootHolder {
    /// An account, whose bootstrap invitation only registering a passkey redeems.
    Passkey,
    /// An API token: a bearer secret with no second factor, for an unattended install or a test.
    Token,
}

/// An account holding a root grant, and the invitation that registers its passkey.
#[derive(Debug)]
pub struct RootInvitation {
    /// The account.
    pub account: Uuid,
    /// The root grant it holds.
    pub grant: Uuid,
    /// The invitation's secret: shown once, stored only as a digest.
    pub secret: Secret,
    /// When the invitation stops working.
    pub expires_at: SystemTime,
}

/// What holds the root grant, and its secret, shown once.
#[derive(Debug)]
pub enum RootCredential {
    /// An account and its bootstrap invitation.
    Invitation(RootInvitation),
    /// An API token and its secret.
    Token(ApiToken, Secret),
}

/// What installation produced, for the operator.
#[derive(Debug)]
pub struct Installation {
    /// The root organization.
    pub organization: Organization,
    /// The root grant.
    pub root_grant: Uuid,
    /// What holds it.
    pub root: RootCredential,
}

impl Installation {
    /// The token holding the root grant and its secret, when a token holds it.
    #[must_use]
    pub const fn token(&self) -> Option<(&ApiToken, &Secret)> {
        match &self.root {
            RootCredential::Token(token, secret) => Some((token, secret)),
            RootCredential::Invitation(_) => None,
        }
    }
}

/// A public key to register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    /// Its identifier, as tokens and assignments name it.
    pub key_id: String,
    /// What it signs: `token` or `assignment`.
    pub purpose: &'static str,
    /// The public half.
    pub public: Vec<u8>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Installs into an empty store, the root grant held as `holder` says. Refused if an
    /// organization already exists.
    pub async fn install(
        &self,
        organization: Name,
        holder: RootHolder,
    ) -> Result<Installation, AppError> {
        let bootstrap = Principal::System(SystemActor::Bootstrap);
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        if tx.any_organization().await? {
            return Err(AppError::conflict(
                "install.done",
                "this store already holds an installation",
            ));
        }
        let org = Organization {
            id: Uuid::now_v7(),
            name: organization,
            parent: None,
            status: OrganizationStatus::Active,
            version: 1,
            created: now,
        };
        tx.insert_organization(&org).await?;
        let created = details([("organization", org.id.to_string().into())]);
        record(&mut tx, &bootstrap, "org.create", created).await?;
        let (root_grant, root) = match holder {
            RootHolder::Passkey => {
                let invitation = root_account(&mut tx, &org, "root", &bootstrap, now).await?;
                (invitation.grant, RootCredential::Invitation(invitation))
            }
            RootHolder::Token => {
                let (grant, token, secret) = root_token(&mut tx, &org, &bootstrap, now).await?;
                (grant, RootCredential::Token(token, secret))
            }
        };
        tx.commit().await?;
        Ok(Installation { organization: org, root_grant, root })
    }

    /// Records the public halves of the signing keys, so verifiers can be given them.
    pub async fn register_keys(&self, keys: &[PublicKey]) -> Result<(), AppError> {
        for k in keys {
            self.store.register_key(&k.key_id, k.purpose, &k.public).await?;
        }
        Ok(())
    }
}

/// Appends an effect of `action` by `actor` with `after` to `tx`.
async fn record<T: Tx>(
    tx: &mut T,
    actor: &Principal,
    action: &str,
    after: serde_json::Map<String, serde_json::Value>,
) -> Result<(), AppError> {
    tx.append_effect(&Effect::new(actor.clone(), action, Outcome::Succeeded).after(after)).await?;
    Ok(())
}

/// Writes `grant`, a root grant, and its effect.
async fn issue_root<T: Tx>(
    tx: &mut T,
    grant: &GrantRecord,
    by: &Principal,
) -> Result<(), AppError> {
    tx.insert_grant(grant).await?;
    let issued = details([
        ("grant", grant.grant.id.to_string().into()),
        ("holder", grant.holder.key().into()),
        ("root", true.into()),
    ]);
    record(tx, by, "grant.issued", issued).await
}

/// An API token holding a new root grant over `org`, issued by `by` at `now`, written in `tx`.
async fn root_token<T: Tx>(
    tx: &mut T,
    org: &Organization,
    by: &Principal,
    now: SystemTime,
) -> Result<(Uuid, ApiToken, Secret), AppError> {
    let secret = Secret::generate().map_err(|e| AppError::Internal(e.to_string()))?;
    let token =
        ApiToken { id: Uuid::now_v7(), name: "root (installation)".into(), expires_at: None };
    tx.insert_token(&token, &secret.digest(), by).await?;
    let grant = root_grant(org, Principal::Token(token.id), by.clone(), now)?;
    issue_root(tx, &grant, by).await?;
    Ok((grant.grant.id, token, secret))
}

/// A new account called `name` holding a new root grant over `org`, with an invitation only
/// registering a passkey redeems, for an hour; issued by `by` at `now`, written in `tx`.
async fn root_account<T: Tx>(
    tx: &mut T,
    org: &Organization,
    name: &str,
    by: &Principal,
    now: SystemTime,
) -> Result<RootInvitation, AppError> {
    let account = Account {
        id: Uuid::now_v7(),
        display_name: name.to_owned(),
        name: None,
        created_by: by.key(),
        created_at: now,
        disabled_at: None,
    };
    let invitation = Invitation {
        id: Uuid::now_v7(),
        account: account.id,
        issued_by: by.key(),
        expires_at: now + BOOTSTRAP_LIFETIME,
        passkey_only: true,
        redeemed_at: None,
    };
    let secret = Secret::generate_kind(SecretKind::Invitation)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    tx.insert_account(&account).await?;
    tx.insert_invitation(&invitation, &secret.digest()).await?;
    let grant = root_grant(org, Principal::User(account.id), by.clone(), now)?;
    issue_root(tx, &grant, by).await?;
    let invited = details([
        ("account", account.id.to_string().into()),
        ("invitation", invitation.id.to_string().into()),
    ]);
    record(tx, by, "account.invited", invited).await?;
    Ok(RootInvitation {
        account: account.id,
        grant: grant.grant.id,
        secret,
        expires_at: invitation.expires_at,
    })
}

/// Every core action over the root organization, with unbounded delegation.
fn root_grant(
    org: &Organization,
    holder: Principal,
    issued_by: Principal,
    now: std::time::SystemTime,
) -> Result<GrantRecord, AppError> {
    let all = CORE
        .iter()
        .map(|a| Action::parse(a).map_err(|e| AppError::Internal(e.to_string())))
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(GrantRecord {
        grant: Grant {
            id: Uuid::now_v7(),
            subject: holder.subject(),
            actions: all.clone(),
            scope: Scope::Object(ObjectRef::new(Kind::Organization, org.id)),
            conditions: Conditions::default(),
            parent: None,
            delegation: Delegation { allowed: true, max_depth: None, actions: all },
            link_form: false,
            uses: 0,
            revoked: false,
            suspended: false,
            irreversible_override: false,
            root: true,
        },
        holder,
        issued_by,
        issued_at: now,
        revoked_at: None,
        suspended_at: None,
        label: "the installation's root grant".to_owned(),
    })
}
