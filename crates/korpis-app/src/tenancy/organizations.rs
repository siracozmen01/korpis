// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged, visible};
use crate::ports::{Clock, EffectWrite, RecipeSource, Store, StoreError, TenancyWrite, Unique};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::tenancy::{Name, Organization, OrganizationStatus};
use uuid::Uuid;

const OPERATION: &str = "tenancy.create_organization";

/// An organization to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOrganization {
    /// Its name, unique among its siblings.
    pub name: Name,
    /// The organization it is delegated from. Required: only installation creates a root
    /// (Rule K-14).
    pub parent: Uuid,
}

fn org(id: Uuid) -> ObjectRef {
    ObjectRef::new(Kind::Organization, id)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Creates an organization under one the caller may create organizations in.
    pub async fn create_organization(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Organization>,
        cmd: CreateOrganization,
    ) -> Result<Reply<Organization>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit = self.authorize(caller, "org.create", org(cmd.parent)).await?;
        let created = Organization {
            id: Uuid::now_v7(),
            name: cmd.name,
            parent: Some(cmd.parent),
            status: OrganizationStatus::Active,
            version: 1,
            created: self.clock.now(),
        };
        let mut tx = self.store.begin().await?;
        tx.insert_organization(&created).await.map_err(|e| match e {
            StoreError::Duplicate(Unique::OrganizationName) => {
                AppError::conflict("organization.name_taken", "the name is taken here")
            }
            other => other.into(),
        })?;
        let made = ObjectRef::new(Kind::Organization, created.id);
        let effect =
            caller.effect("org.create", Outcome::Succeeded).under(permit.grant).about(made).after(
                details([
                    ("organization", created.id.to_string().into()),
                    ("parent", cmd.parent.to_string().into()),
                    ("name", created.name.as_str().into()),
                ]),
            );
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, OPERATION, created).await
    }

    /// One organization the caller may read.
    pub async fn get_organization(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<Organization, AppError> {
        self.authorize(caller, "org.read", org(id)).await?;
        self.store.organization(id).await?.ok_or(AppError::NotFound { kind: "organization" })
    }

    /// The organizations under `parent`, which the caller must be able to read; without a parent,
    /// the roots the caller may read. Existence is gated by read authority (§8 of
    /// docs/10-api.md), so a root the caller may not read is not listed.
    pub async fn list_organizations(
        &self,
        caller: &Caller,
        parent: Option<Uuid>,
        page: Page,
    ) -> Result<Paged<Organization>, AppError> {
        if let Some(p) = parent {
            self.authorize(caller, "org.read", org(p)).await?;
        }
        let read = |p: Page| async move { Ok(self.store.organizations(parent, p).await?) };
        let readable = |rows: Vec<Organization>| async move {
            if parent.is_some() {
                return Ok(rows);
            }
            let objects: Vec<ObjectRef> = rows.iter().map(|o| org(o.id)).collect();
            let shown = self.permitted_each(caller, "org.read", &objects).await?;
            Ok(rows.into_iter().zip(shown).filter_map(|(o, s)| s.then_some(o)).collect())
        };
        visible(page, |o| o.id, read, readable).await
    }
}
