// SPDX-License-Identifier: AGPL-3.0-or-later

//! A root organization's extension policy: read under `org.read` and set under `org.write` there,
//! by compare-and-swap, every change an effect; and enforced on every install and upgrade beneath
//! it, a dry run included, before anything else of the extension is believed. A refusal is
//! `extension.not_allowed` and says which rule refused.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    Clock, EffectWrite as _, ExtensionPolicyWrite as _, RecipeSource, SetExtensionPolicy, Store,
    Tx as _,
};
use crate::tenancy::signers_validated;
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::extension::policy::{ExtensionPolicy, MAX_REGISTRIES};
use korpis_core::recipe::RecipeRef;
use korpis_core::recipe::trust::TrustPolicy;
use uuid::Uuid;

const OPERATION: &str = "extension.set_policy";

/// The policy a root organization governs its tree by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoverningExtensions {
    /// The root organization.
    pub organization: Uuid,
    /// The policy in force: the one set, or the default.
    pub policy: ExtensionPolicy,
    /// Its version, zero when never set: what the next change must expect.
    pub version: u64,
}

/// A change to a root organization's extension policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetPolicy {
    /// The root organization.
    pub organization: Uuid,
    /// The policy; publisher fingerprints are computed here, whatever the caller sent.
    pub policy: ExtensionPolicy,
    /// The version as the caller saw it; zero if none was set.
    pub expected_version: u64,
}

fn refused(message: impl Into<String>) -> AppError {
    AppError::conflict("extension.not_allowed", message)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Refuses `organization` unless it is a root: only a root's policy governs.
    async fn root(&self, organization: Uuid) -> Result<(), AppError> {
        let org = self.store.organization(organization).await?;
        match org {
            None => Err(AppError::NotFound { kind: "organization" }),
            Some(o) if o.parent.is_some() => Err(AppError::invalid(
                "organization",
                "a root organization: its extension policy governs every organization beneath it",
            )),
            Some(_) => Ok(()),
        }
    }

    /// The extension policy of the root organization `organization`.
    pub async fn get_extension_policy(
        &self,
        caller: &Caller,
        organization: Uuid,
    ) -> Result<GoverningExtensions, AppError> {
        self.authorize(caller, "org.read", ObjectRef::new(Kind::Organization, organization))
            .await?;
        self.root(organization).await?;
        Ok(self.governing_extensions(organization).await?)
    }

    async fn governing_extensions(&self, root: Uuid) -> Result<GoverningExtensions, AppError> {
        let set = self.store.extension_policy(root).await?;
        Ok(GoverningExtensions {
            organization: root,
            policy: set.as_ref().map(|s| s.policy.clone()).unwrap_or_default(),
            version: set.map_or(0, |s| s.version),
        })
    }

    /// Sets a root organization's extension policy.
    pub async fn set_extension_policy(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, GoverningExtensions>,
        cmd: SetPolicy,
    ) -> Result<Reply<GoverningExtensions>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        caller.direct()?;
        let object = ObjectRef::new(Kind::Organization, cmd.organization);
        let permit = self.authorize(caller, "org.write", object).await?;
        self.root(cmd.organization).await?;
        let mut policy = cmd.policy;
        if let Err(i) = policy.check_registries() {
            let why = format!("at most {MAX_REGISTRIES}, each a lowercase host or path under one");
            return Err(AppError::invalid(format!("policy.registries[{i}]"), why));
        }
        policy.publishers = signers_validated("policy.publishers", &policy.publishers)?;
        let set = SetExtensionPolicy {
            organization: cmd.organization,
            policy,
            version: cmd.expected_version.saturating_add(1),
            set_by: caller.principal.clone(),
            set_at: self.clock.now(),
        };
        let mut tx = self.store.begin().await?;
        if !tx.set_extension_policy(&set, cmd.expected_version).await? {
            let message = "the organization's extension policy changed meanwhile";
            return Err(AppError::conflict("extension_policy.moved", message));
        }
        let publishers: Vec<serde_json::Value> = set
            .policy
            .publishers
            .iter()
            .map(|p| serde_json::json!({"name": p.name, "fingerprint": p.fingerprint}))
            .collect();
        let effect = caller
            .effect("extension.policy.set", Outcome::Succeeded)
            .under(permit.grant)
            .about(object)
            .after(details([
                ("version", set.version.into()),
                ("registries", set.policy.registries.clone().into()),
                ("publishers", publishers.into()),
                ("tenants_install", set.policy.tenants_install.into()),
            ]));
        tx.append_effect(&effect).await?;
        let governing = GoverningExtensions {
            organization: set.organization,
            policy: set.policy,
            version: set.version,
        };
        finish(tx, caller, idem, OPERATION, governing).await
    }

    /// Refuses installing `artifact` into `organization` for `caller` where the policy of the
    /// root above it does not allow it: a registry it does not list, no listed publisher's
    /// signature over the digest, or an installer without `extension.install` over the root
    /// when tenants may not install.
    pub(super) async fn extension_allowed(
        &self,
        caller: &Caller,
        organization: Uuid,
        artifact: &RecipeRef,
    ) -> Result<(), AppError> {
        let chain = self.organization_chain(organization).await?;
        let root = *chain.last().ok_or(AppError::NotFound { kind: "organization" })?;
        let policy = self.governing_extensions(root).await?.policy;
        if !policy.admits_registry(&artifact.name) {
            return Err(refused(format!(
                "{} is not from a registry the operator allows: {}",
                artifact.name,
                policy.registries.join(", ")
            )));
        }
        if !policy.publishers.is_empty() {
            let publishers = TrustPolicy {
                accepted_signers: policy.publishers.clone(),
                ..TrustPolicy::default()
            };
            if self.vouch(&artifact.name, &artifact.digest, &publishers).await.is_err() {
                let names: Vec<&str> = policy.publishers.iter().map(|p| p.name.as_str()).collect();
                return Err(refused(format!(
                    "{} is signed by none of the publishers the operator allows: {}",
                    artifact.digest,
                    names.join(", ")
                )));
            }
        }
        let over_root = ObjectRef::new(Kind::Organization, root);
        if !policy.tenants_install && !self.permitted(caller, "extension.install", over_root).await?
        {
            return Err(refused(
                "the operator installs extensions here: installing one needs extension.install \
                 over the root organization",
            ));
        }
        Ok(())
    }
}
