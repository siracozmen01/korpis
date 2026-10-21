// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipe trust policies (§7 of docs/09-recipes.md): an organization sets its own, or is
//! governed by its nearest ancestor's, or by the default, which runs nothing.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, PolicyChange, RecipeSource, SetPolicy, Store, TrustWrite};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::recipe::trust::cosign::signer;
use korpis_core::recipe::trust::{Signer, TrustPolicy};
use std::collections::BTreeSet;
use uuid::Uuid;

const OPERATION: &str = "tenancy.set_trust_policy";

/// The most signers one policy names: each is checked against every signature of every recipe
/// the organization declares from, so the bound is on that cost, not on what may be trusted.
const MAX_SIGNERS: usize = 64;
/// The longest signer name, in bytes.
const MAX_NAME: usize = 128;

/// The policy that governs an organization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Governing {
    /// The policy and the organization that set it; `None` when the default governs.
    pub set: Option<SetPolicy>,
    /// The version of the organization's own policy, zero when it never set one: what the next
    /// change must expect.
    pub own_version: u64,
}

impl Governing {
    /// The policy in force: the one set, or the default.
    #[must_use]
    pub fn policy(&self) -> TrustPolicy {
        self.set.as_ref().map(|s| s.policy.clone()).unwrap_or_default()
    }
}

/// A change to an organization's own policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetTrustPolicy {
    /// The organization.
    pub organization: Uuid,
    /// Its policy; signer fingerprints are computed here, whatever the caller sent. `None`
    /// removes its own, so its nearest ancestor's governs again.
    pub policy: Option<TrustPolicy>,
    /// The organization's own version as the caller saw it; zero if it has none.
    pub expected_version: u64,
}

fn org(id: Uuid) -> ObjectRef {
    ObjectRef::new(Kind::Organization, id)
}

/// The policy with every signer's key parsed and fingerprinted, names unique and keys too.
fn validated(policy: &TrustPolicy) -> Result<TrustPolicy, AppError> {
    let field = |i: usize, f: &str| format!("policy.accepted_signers[{i}].{f}");
    if policy.accepted_signers.len() > MAX_SIGNERS {
        let message = format!("at most {MAX_SIGNERS} signers");
        return Err(AppError::invalid("policy.accepted_signers", message));
    }
    let (mut names, mut keys) = (BTreeSet::new(), BTreeSet::new());
    let mut signers: Vec<Signer> = Vec::with_capacity(policy.accepted_signers.len());
    for (i, s) in policy.accepted_signers.iter().enumerate() {
        let bad_name = s.name.is_empty() || s.name.len() > MAX_NAME;
        if bad_name || s.name.chars().any(char::is_control) {
            let message = format!("1 to {MAX_NAME} bytes, no control characters");
            return Err(AppError::invalid(field(i, "name"), message));
        }
        if !names.insert(s.name.clone()) {
            return Err(AppError::invalid(field(i, "name"), "named twice"));
        }
        let parsed = signer(&s.name, &s.public_key_pem)
            .map_err(|e| AppError::invalid(field(i, "public_key_pem"), e.to_string()))?;
        if !keys.insert(parsed.fingerprint.clone()) {
            return Err(AppError::invalid(field(i, "public_key_pem"), "this key is named twice"));
        }
        signers.push(parsed);
    }
    Ok(TrustPolicy {
        accepted_signers: signers,
        allow_unsigned: policy.allow_unsigned,
        allow_unverified: policy.allow_unverified,
    })
}

fn effect_details(change: &PolicyChange) -> serde_json::Map<String, serde_json::Value> {
    let signers: Vec<serde_json::Value> = change
        .policy
        .iter()
        .flat_map(|p| {
            p.accepted_signers
                .iter()
                .map(|s| serde_json::json!({"name": s.name, "fingerprint": s.fingerprint}))
        })
        .collect();
    details([
        ("organization", change.organization.to_string().into()),
        ("version", change.version.into()),
        ("removed", change.policy.is_none().into()),
        ("accepted_signers", signers.into()),
        ("allow_unsigned", change.policy.as_ref().is_some_and(|p| p.allow_unsigned).into()),
        ("allow_unverified", change.policy.as_ref().is_some_and(|p| p.allow_unverified).into()),
    ])
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The policy that governs `organization`, which the caller may read.
    pub async fn get_trust_policy(
        &self,
        caller: &Caller,
        organization: Uuid,
    ) -> Result<Governing, AppError> {
        self.authorize(caller, "org.read", org(organization)).await?;
        self.store
            .organization(organization)
            .await?
            .ok_or(AppError::NotFound { kind: "organization" })?;
        let set = self.store.trust_policy(organization).await?;
        let own_version = self.store.own_trust_version(organization).await?;
        Ok(Governing { set, own_version })
    }

    /// Sets or removes `organization`'s own policy. It governs every recipe the organization and
    /// its descendants without a policy of their own declare from, from the next declaration.
    pub async fn set_trust_policy(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Governing>,
        cmd: SetTrustPolicy,
    ) -> Result<Reply<Governing>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit = self.authorize(caller, "org.write", org(cmd.organization)).await?;
        let organization = self
            .store
            .organization(cmd.organization)
            .await?
            .ok_or(AppError::NotFound { kind: "organization" })?;
        if cmd.policy.is_none() && cmd.expected_version == 0 {
            let message = "the organization has no policy of its own to remove";
            return Err(AppError::conflict("trust_policy.not_set", message));
        }
        let change = PolicyChange {
            organization: organization.id,
            policy: cmd.policy.as_ref().map(validated).transpose()?,
            version: cmd.expected_version.saturating_add(1),
            set_by: caller.principal.clone(),
            set_at: self.clock.now(),
        };
        let set = match &change.policy {
            Some(policy) => Some(SetPolicy {
                organization: organization.id,
                policy: policy.clone(),
                version: change.version,
                set_by: change.set_by.clone(),
                set_at: change.set_at,
            }),
            None => match organization.parent {
                Some(parent) => self.store.trust_policy(parent).await?,
                None => None,
            },
        };
        let mut tx = self.store.begin().await?;
        if !tx.set_trust_policy(&change, cmd.expected_version).await? {
            let message = "the organization's policy changed meanwhile";
            return Err(AppError::conflict("trust_policy.moved", message));
        }
        let effect = caller
            .effect("org.trust_policy.set", Outcome::Succeeded)
            .under(permit.grant)
            .about(ObjectRef::new(Kind::Organization, change.organization))
            .after(effect_details(&change));
        tx.append_effect(&effect).await?;
        let governing = Governing { set, own_version: change.version };
        finish(tx, caller, idem, OPERATION, governing).await
    }
}
