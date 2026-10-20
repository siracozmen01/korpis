// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity bindings (§5 of docs/08-identity.md): declared, edited, and retired under
//! `identity.binding.write` on the organization their provider is installed into, and read there
//! under `org.read`. A binding grants nothing; it decides which provider vouches for a subject,
//! which is why declaring one takes authority over the whole organization that provider serves.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{
    BindingWrite as _, Clock, EffectWrite as _, RecipeSource, Store, StoreError, Unique,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::binding::{
    BindingOrigin, BindingState, BindingTerms, IdentityBinding, ProviderRef,
};
use korpis_core::authz::{Kind, ObjectRef, Permit};
use korpis_core::extension::ProviderType;
use korpis_core::principal::Principal;
use uuid::Uuid;

const CREATE: &str = "authz.create_identity_binding";
const UPDATE: &str = "authz.update_identity_binding";
const RETIRE: &str = "authz.retire_identity_binding";
const KIND: &str = "identity binding";
const WRITE: &str = "identity.binding.write";

/// A binding as an operator declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingAsked {
    /// The identity provider vouching for it.
    pub provider: ProviderRef,
    /// The external identity it binds.
    pub subject: Principal,
    /// What it says.
    pub terms: BindingTerms,
}

fn organization(id: Uuid) -> ObjectRef {
    ObjectRef::new(Kind::Organization, id)
}

fn not_found() -> AppError {
    AppError::NotFound { kind: KIND }
}

fn bound(e: StoreError) -> AppError {
    match e {
        StoreError::Duplicate(Unique::BindingSubject) => {
            AppError::conflict("binding.exists", "the subject is bound already; edit that binding")
        }
        other => other.into(),
    }
}

fn effect(caller: &Caller, action: &str, permit: &Permit, b: &IdentityBinding) -> Effect {
    Effect::new(caller.principal.clone(), action, Outcome::Succeeded).under(permit.grant).after(
        details([
            ("binding", b.id.to_string().into()),
            ("organization", b.provider.organization.to_string().into()),
            ("provider", b.provider.name.clone().into()),
            ("subject", b.subject.key().into()),
            ("version", b.version.into()),
            ("allows_irreversible", b.terms.allows_irreversible.into()),
            ("template", b.terms.template.map(|t| t.template.to_string()).into()),
            ("template_version", b.terms.template.map(|t| t.version).into()),
        ]),
    )
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Checks `binding` against its definition, its provider, and its template.
    async fn binding_checked(&self, binding: &IdentityBinding) -> Result<(), AppError> {
        binding.validate().map_err(|e| AppError::invalid(e.field(), e.to_string()))?;
        let p = &binding.provider;
        let installed =
            self.store.providing_installation(p.organization, ProviderType::Identity, &p.name);
        if installed.await?.is_none() {
            let why = "no identity provider of that name is installed into the organization";
            return Err(AppError::invalid("provider", why));
        }
        let Some(r) = binding.terms.template else { return Ok(()) };
        // What a set's members receive is a template offered where the binding is declared,
        // at the version the declarer read, so an edit of it reaches the set only by an edit
        // of the binding.
        let template = self.store.template(r.template).await?;
        let owners = self.organizations_above(organization(p.organization)).await?;
        let template = template
            .filter(|t| t.retired_at.is_none() && t.offered_within(&owners))
            .ok_or(AppError::NotFound { kind: "grant template" })?;
        if template.current.version != r.version {
            let why = format!("the template is at version {}", template.current.version);
            return Err(AppError::conflict("template.changed", why));
        }
        Ok(())
    }

    /// Declares a binding.
    pub async fn create_identity_binding(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, IdentityBinding>,
        asked: BindingAsked,
    ) -> Result<Reply<IdentityBinding>, AppError> {
        if let Some(r) = self.replay(caller, idem, CREATE).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit =
            self.authorize(caller, WRITE, organization(asked.provider.organization)).await?;
        let made = IdentityBinding {
            id: Uuid::now_v7(),
            provider: asked.provider,
            subject: asked.subject,
            origin: BindingOrigin::Operator,
            terms: asked.terms,
            // Nothing has confirmed it yet: grants rooted in it wait for its provider (§5.1).
            state: BindingState::Unverified,
            last_verified_at: None,
            verifications: 0,
            version: 1,
            made_by: caller.principal.clone(),
            made_at: self.clock.now(),
            retired_at: None,
        };
        self.binding_checked(&made).await?;
        let mut tx = self.store.begin().await?;
        tx.insert_binding(&made).await.map_err(bound)?;
        tx.append_effect(&effect(caller, "identity.binding.create", &permit, &made)).await?;
        finish(tx, caller, idem, CREATE, made).await
    }

    /// A live binding the caller may change; one the caller may not see does not exist.
    pub(super) async fn binding_to_write(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<(IdentityBinding, Permit), AppError> {
        let binding = self.store.binding(id).await?.ok_or_else(not_found)?;
        let permit = self
            .authorize(caller, WRITE, organization(binding.provider.organization))
            .await
            .map_err(|e| if matches!(e, AppError::NotFound { .. }) { not_found() } else { e })?;
        if binding.retired_at.is_some() {
            let why = "the binding is retired; declare another";
            return Err(AppError::conflict("binding.retired", why));
        }
        Ok((binding, permit))
    }

    /// Replaces a binding's terms, against the version the edit was made from.
    pub async fn update_identity_binding(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, IdentityBinding>,
        id: Uuid,
        expected: u64,
        terms: BindingTerms,
    ) -> Result<Reply<IdentityBinding>, AppError> {
        if let Some(r) = self.replay(caller, idem, UPDATE).await? {
            return Ok(Reply::Replayed(r));
        }
        let (mut binding, permit) = self.binding_to_write(caller, id).await?;
        let changed = || {
            let why = "the binding changed since that version; read it again";
            AppError::conflict("binding.changed", why)
        };
        if binding.version != expected {
            return Err(changed());
        }
        binding.terms = terms;
        binding.version = expected + 1;
        self.binding_checked(&binding).await?;
        let mut tx = self.store.begin().await?;
        if !tx.update_binding((id, expected), &binding.terms, self.clock.now()).await? {
            return Err(changed());
        }
        binding.verifications += 1;
        tx.append_effect(&effect(caller, "identity.binding.update", &permit, &binding)).await?;
        finish(tx, caller, idem, UPDATE, binding).await
    }

    /// Retires a binding: it vouches for nothing since, so every grant rooted in it is
    /// suspended, and its subject may be bound again.
    pub async fn retire_identity_binding(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, IdentityBinding>,
        id: Uuid,
    ) -> Result<Reply<IdentityBinding>, AppError> {
        if let Some(r) = self.replay(caller, idem, RETIRE).await? {
            return Ok(Reply::Replayed(r));
        }
        let (mut binding, permit) = self.binding_to_write(caller, id).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        if !tx.retire_binding(id, now).await? {
            return Err(AppError::conflict("binding.retired", "the binding is retired already"));
        }
        binding.retired_at = Some(now);
        tx.append_effect(&effect(caller, "identity.binding.retire", &permit, &binding)).await?;
        finish(tx, caller, idem, RETIRE, binding).await
    }

    /// A binding, read under `org.read` where it is declared.
    pub async fn get_identity_binding(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<IdentityBinding, AppError> {
        let binding = self.store.binding(id).await?.ok_or_else(not_found)?;
        let at = organization(binding.provider.organization);
        if !self.permitted(caller, "org.read", at).await? {
            return Err(not_found());
        }
        Ok(binding)
    }

    /// The bindings declared in an organization, under `org.read` there.
    pub async fn list_identity_bindings(
        &self,
        caller: &Caller,
        org: Uuid,
        retired: bool,
        page: Page,
    ) -> Result<Paged<IdentityBinding>, AppError> {
        self.authorize(caller, "org.read", organization(org)).await?;
        let items = self.store.bindings_in(org, retired, page).await?;
        let next = page.next(&items, |b| b.id);
        Ok(Paged { items, next })
    }
}
