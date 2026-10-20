// SPDX-License-Identifier: AGPL-3.0-or-later

//! Projects.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{Clock, EffectWrite, RecipeSource, Store, StoreError, TenancyWrite, Unique};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::labels::Labels;
use korpis_core::tenancy::{Name, Project};
use uuid::Uuid;

const OPERATION: &str = "tenancy.create_project";

/// A project to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateProject {
    /// The organization it belongs to.
    pub organization: Uuid,
    /// Its name, unique within the organization.
    pub name: Name,
    /// For selectors.
    pub labels: Labels,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Creates a project in an organization the caller may create projects in.
    pub async fn create_project(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Project>,
        cmd: CreateProject,
    ) -> Result<Reply<Project>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let org = ObjectRef::new(Kind::Organization, cmd.organization);
        let permit = self.authorize(caller, "project.create", org).await?;
        let created = Project {
            id: Uuid::now_v7(),
            organization: cmd.organization,
            name: cmd.name,
            labels: cmd.labels,
            version: 1,
            created: self.clock.now(),
        };
        let mut tx = self.store.begin().await?;
        tx.insert_project(&created).await.map_err(|e| match e {
            StoreError::Duplicate(Unique::ProjectName) => {
                AppError::conflict("project.name_taken", "the name is taken here")
            }
            other => other.into(),
        })?;
        let effect = caller.effect("project.create", Outcome::Succeeded).under(permit.grant).after(
            details([
                ("project", created.id.to_string().into()),
                ("organization", cmd.organization.to_string().into()),
                ("name", created.name.as_str().into()),
            ]),
        );
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, OPERATION, created).await
    }

    /// One project the caller may read.
    pub async fn get_project(&self, caller: &Caller, id: Uuid) -> Result<Project, AppError> {
        self.authorize(caller, "project.read", ObjectRef::new(Kind::Project, id)).await?;
        self.store.project(id).await?.ok_or(AppError::NotFound { kind: "project" })
    }

    /// The projects of an organization whose projects the caller may read.
    pub async fn list_projects(
        &self,
        caller: &Caller,
        organization: Uuid,
        page: Page,
    ) -> Result<Paged<Project>, AppError> {
        let org = ObjectRef::new(Kind::Organization, organization);
        self.authorize(caller, "project.read", org).await?;
        let items = self.store.projects(organization, page).await?;
        let next = page.next(&items, |p| p.id);
        Ok(Paged { items, next })
    }
}
