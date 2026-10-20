// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grant templates (§4 of docs/08-identity.md): made, edited, and retired under
//! `template.write` on the organization that owns them, and read wherever they are offered, at
//! the owner and beneath it. A template carries no authority, so managing one is the owner's own
//! business rather than an authority from above; applying one is attenuating a grant the issuer
//! holds, which issuing decides.

use crate::App;
use crate::authz::gateway::read_namespace;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{
    Clock, EffectWrite as _, RecipeSource, Store, StoreError, TemplateWrite as _, Unique,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::template::{GrantTemplate, TemplateBody, TemplateVersion};
use korpis_core::authz::{Kind, ObjectRef, Permit};
use korpis_core::tenancy::Name;
use uuid::Uuid;

const CREATE: &str = "authz.create_grant_template";
const UPDATE: &str = "authz.update_grant_template";
const RETIRE: &str = "authz.retire_grant_template";
const KIND: &str = "grant template";

/// A template as it is asked for: its name and what it asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateAsked {
    /// Its name, unique among its organization's live templates.
    pub name: Name,
    /// What it asks for.
    pub body: TemplateBody,
}

/// Which templates to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplatesOf {
    /// The live templates offered at an object: owned by it or by any organization above it.
    At(ObjectRef),
    /// The templates an organization owns, retired ones included.
    Owned(Uuid),
}

fn organization(id: Uuid) -> ObjectRef {
    ObjectRef::new(Kind::Organization, id)
}

fn not_found() -> AppError {
    AppError::NotFound { kind: KIND }
}

fn checked(asked: &TemplateAsked) -> Result<(), AppError> {
    asked.body.validate().map_err(|e| AppError::invalid(e.field(), e.to_string()))
}

fn name_taken(e: StoreError) -> AppError {
    match e {
        StoreError::Duplicate(Unique::TemplateName) => AppError::conflict(
            "template.name_taken",
            "a live template of this organization has that name",
        ),
        other => other.into(),
    }
}

fn effect(caller: &Caller, action: &str, permit: &Permit, t: &GrantTemplate) -> Effect {
    caller.effect(action, Outcome::Succeeded).under(permit.grant).after(details([
        ("template", t.id.to_string().into()),
        ("organization", t.organization.to_string().into()),
        ("name", t.current.name.as_str().into()),
        ("version", t.current.version.into()),
    ]))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Makes a template, owned by `owner`.
    pub async fn create_grant_template(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, GrantTemplate>,
        owner: Uuid,
        asked: TemplateAsked,
    ) -> Result<Reply<GrantTemplate>, AppError> {
        if let Some(r) = self.replay(caller, idem, CREATE).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit = self.authorize(caller, "template.write", organization(owner)).await?;
        checked(&asked)?;
        let made = GrantTemplate {
            id: Uuid::now_v7(),
            organization: owner,
            current: TemplateVersion {
                version: 1,
                name: asked.name,
                body: asked.body,
                by: caller.principal.clone(),
                at: self.clock.now(),
            },
            retired_at: None,
        };
        let mut tx = self.store.begin().await?;
        tx.insert_template(&made).await.map_err(name_taken)?;
        tx.append_effect(&effect(caller, "template.create", &permit, &made)).await?;
        finish(tx, caller, idem, CREATE, made).await
    }

    /// A template the caller may change: `template.write` on its owner, and a template the
    /// caller may not see at all is one that does not exist.
    async fn template_to_write(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<(GrantTemplate, Permit), AppError> {
        let template = self.store.template(id).await?.ok_or_else(not_found)?;
        let permit = self
            .authorize(caller, "template.write", organization(template.organization))
            .await
            .map_err(|e| if matches!(e, AppError::NotFound { .. }) { not_found() } else { e })?;
        if template.retired_at.is_some() {
            let why = "the template is retired; make another";
            return Err(AppError::conflict("template.retired", why));
        }
        Ok((template, permit))
    }

    /// Edits a template against the version the edit was made from: its next version.
    pub async fn update_grant_template(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, GrantTemplate>,
        id: Uuid,
        expected: u64,
        asked: TemplateAsked,
    ) -> Result<Reply<GrantTemplate>, AppError> {
        if let Some(r) = self.replay(caller, idem, UPDATE).await? {
            return Ok(Reply::Replayed(r));
        }
        let (mut template, permit) = self.template_to_write(caller, id).await?;
        checked(&asked)?;
        let changed = || {
            let why = "the template changed since that version; read it again";
            AppError::conflict("template.changed", why)
        };
        if template.current.version != expected {
            return Err(changed());
        }
        template.current = TemplateVersion {
            version: expected + 1,
            name: asked.name,
            body: asked.body,
            by: caller.principal.clone(),
            at: self.clock.now(),
        };
        let mut tx = self.store.begin().await?;
        if !tx.update_template(id, expected, &template.current).await.map_err(name_taken)? {
            return Err(changed());
        }
        tx.append_effect(&effect(caller, "template.update", &permit, &template)).await?;
        finish(tx, caller, idem, UPDATE, template).await
    }

    /// Retires a template: offered nowhere since, and kept, because grants name its versions.
    pub async fn retire_grant_template(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, GrantTemplate>,
        id: Uuid,
    ) -> Result<Reply<GrantTemplate>, AppError> {
        if let Some(r) = self.replay(caller, idem, RETIRE).await? {
            return Ok(Reply::Replayed(r));
        }
        let (mut template, permit) = self.template_to_write(caller, id).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        if !tx.retire_template(id, now).await? {
            let why = "the template is retired already";
            return Err(AppError::conflict("template.retired", why));
        }
        template.retired_at = Some(now);
        tx.append_effect(&effect(caller, "template.retire", &permit, &template)).await?;
        finish(tx, caller, idem, RETIRE, template).await
    }

    /// The organizations at and above `at`, nearest first, once the caller has shown it may
    /// read `at`.
    async fn owners_above(&self, caller: &Caller, at: ObjectRef) -> Result<Vec<Uuid>, AppError> {
        self.authorize(caller, &format!("{}.read", read_namespace(at.kind)), at).await?;
        self.organizations_above(at).await
    }

    /// A template, at `version` or its current one: read at its owner under `org.read`, or,
    /// with `at`, wherever it is offered under reading that object.
    pub async fn get_grant_template(
        &self,
        caller: &Caller,
        id: Uuid,
        version: Option<u64>,
        at: Option<ObjectRef>,
    ) -> Result<GrantTemplate, AppError> {
        // The object is read before the template is looked for, so whether the template exists
        // is never said to a caller who may not read where it is asked about.
        let owners = match at {
            Some(at) => Some(self.owners_above(caller, at).await?),
            None => None,
        };
        let found = self.store.template(id).await?;
        let shown = match (found, owners) {
            (Some(t), Some(owners)) => t.offered_within(&owners).then_some(t),
            (Some(t), None) => {
                let read = self.permitted(caller, "org.read", organization(t.organization)).await?;
                read.then_some(t)
            }
            (None, _) => None,
        };
        let mut template = shown.ok_or_else(not_found)?;
        if let Some(v) = version.filter(|v| *v != template.current.version) {
            template.current = self.store.template_version(id, v).await?.ok_or_else(not_found)?;
        }
        Ok(template)
    }

    /// The templates offered at an object, or owned by an organization.
    pub async fn list_grant_templates(
        &self,
        caller: &Caller,
        of: TemplatesOf,
        page: Page,
    ) -> Result<Paged<GrantTemplate>, AppError> {
        let (owners, live) = match of {
            TemplatesOf::At(at) => (self.owners_above(caller, at).await?, true),
            TemplatesOf::Owned(org) => {
                self.authorize(caller, "org.read", organization(org)).await?;
                (vec![org], false)
            }
        };
        let items = self.store.templates_of(&owners, live, page).await?;
        let next = page.next(&items, |t| t.id);
        Ok(Paged { items, next })
    }
}
