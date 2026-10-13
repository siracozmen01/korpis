// SPDX-License-Identifier: AGPL-3.0-or-later

//! Installation: the first organization and the first grant (§9.1 of docs/08-identity.md).
//! Every other grant descends from the one created here.
//!
//! In Phase 0 the root grant goes to an API token shown once, because there is no web client yet
//! to register the passkey §9.1 asks for. When the web client arrives the bootstrap token of §9.1
//! replaces this path, and an installation's Phase 0 root token can be revoked once a
//! passkey-held root grant exists.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, CredentialWrite, EffectWrite, GrantWrite, RecipeSource, Store, TenancyWrite, Tx,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::action::CORE;
use korpis_core::authz::credential::{ApiToken, Secret};
use korpis_core::authz::{
    Action, Conditions, Delegation, Grant, GrantRecord, Kind, ObjectRef, Scope,
};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::tenancy::{Name, Organization, OrganizationStatus};
use std::collections::BTreeSet;
use uuid::Uuid;

/// What installation produced, for the operator.
#[derive(Debug)]
pub struct Installation {
    /// The root organization.
    pub organization: Organization,
    /// The root grant.
    pub root_grant: Uuid,
    /// The token holding it.
    pub token: ApiToken,
    /// The token's secret: shown once, stored only as a digest.
    pub secret: Secret,
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
    /// Installs into an empty store. Refused if an organization already exists.
    pub async fn install(&self, organization: Name) -> Result<Installation, AppError> {
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

        let secret = Secret::generate().map_err(|e| AppError::Internal(e.to_string()))?;
        let token =
            ApiToken { id: Uuid::now_v7(), name: "root (installation)".into(), expires_at: None };
        tx.insert_token(&token, &secret.digest(), &bootstrap).await?;

        let root = root_grant(&org, Principal::Token(token.id), bootstrap.clone(), now)?;
        tx.insert_grant(&root).await?;
        for (action, after) in [
            ("org.create", details([("organization", org.id.to_string().into())])),
            (
                "grant.issued",
                details([("grant", root.grant.id.to_string().into()), ("root", true.into())]),
            ),
        ] {
            tx.append_effect(
                &Effect::new(bootstrap.clone(), action, Outcome::Succeeded).after(after),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(Installation { organization: org, root_grant: root.grant.id, token, secret })
    }

    /// Records the public halves of the signing keys, so verifiers can be given them.
    pub async fn register_keys(&self, keys: &[PublicKey]) -> Result<(), AppError> {
        for k in keys {
            self.store.register_key(&k.key_id, k.purpose, &k.public).await?;
        }
        Ok(())
    }
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
