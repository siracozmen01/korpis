// SPDX-License-Identifier: AGPL-3.0-or-later

//! Repositories (§5.3 of docs/06-storage.md): made under `repository.write` on the project or
//! organization they serve, each with a key-encryption key the vault generates and keeps sealed.
//! A repository names its backup target provider by name, as calls do; one need not be serving
//! when the repository is made, and a backup through it waits for one as any call does.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{
    BackupWrite as _, Clock, EffectWrite as _, RecipeSource, Store, StoreError, Unique,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::backup::{KeyCustody, Repository, Retention};
use korpis_core::extension::is_prefix;
use korpis_core::object::{Kind, ObjectRef};
use korpis_core::tenancy::Name;
use uuid::Uuid;

const OPERATION: &str = "backup.create_repository";

/// What a new repository is.
#[derive(Debug, Clone)]
pub struct CreateRepository {
    /// The organization it belongs to.
    pub organization: Uuid,
    /// The project it serves; the whole organization when none.
    pub project: Option<Uuid>,
    /// Its name.
    pub name: Name,
    /// The organization's backup target provider, by name.
    pub provider: Name,
    /// Its place in the target.
    pub prefix: String,
    /// Who holds its key.
    pub custody: KeyCustody,
    /// How long its backups are kept.
    pub retention: Retention,
    /// Whether it takes the streams of the workloads it serves; when not said, it does unless
    /// another repository already takes them for the same organization or project.
    pub streams: Option<bool>,
}

/// That `cmd` asks for a repository that can be made.
fn checked(cmd: &CreateRepository) -> Result<(), AppError> {
    if !is_prefix(&cmd.prefix) {
        return Err(AppError::invalid(
            "prefix",
            "a relative key prefix ending in /, of letters, digits, '.', '_', and '-'",
        ));
    }
    if !cmd.retention.keeps_something() {
        return Err(AppError::invalid("retention", "a policy that keeps at least one backup"));
    }
    if cmd.custody == KeyCustody::Tenant {
        let why = "tenant custody needs a key management provider, which is not built yet";
        return Err(AppError::conflict("repository.custody_unavailable", why));
    }
    Ok(())
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The vault, or why there is none.
    pub(crate) fn vault(&self) -> Result<&dyn crate::ports::KeyVault, AppError> {
        self.vault.as_deref().ok_or_else(|| AppError::Internal("no key vault".into()))
    }

    /// The object a new repository is authorized on: its project, or its organization.
    async fn repository_scope(&self, cmd: &CreateRepository) -> Result<ObjectRef, AppError> {
        let Some(project) = cmd.project else {
            return Ok(ObjectRef::new(Kind::Organization, cmd.organization));
        };
        let p = self.store.project(project).await?.ok_or(AppError::NotFound { kind: "project" })?;
        if p.organization != cmd.organization {
            return Err(AppError::invalid("project", "a project of the organization"));
        }
        Ok(ObjectRef::new(Kind::Project, project))
    }

    /// Makes a repository.
    pub async fn create_repository(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Repository>,
        cmd: CreateRepository,
    ) -> Result<Reply<Repository>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit =
            self.authorize(caller, "repository.write", self.repository_scope(&cmd).await?).await?;
        checked(&cmd)?;
        let streams = match cmd.streams {
            Some(s) => s,
            None => !self.store.streams_taken(cmd.organization, cmd.project).await?,
        };
        let created = Repository {
            id: Uuid::now_v7(),
            organization: cmd.organization,
            project: cmd.project,
            name: cmd.name.as_str().to_owned(),
            provider: cmd.provider.as_str().to_owned(),
            prefix: cmd.prefix,
            custody: cmd.custody,
            retention: cmd.retention,
            streams,
            created: self.clock.now(),
            deleted_at: None,
        };
        let kek =
            self.vault()?.new_kek(created.id).map_err(|e| AppError::Internal(e.to_string()))?;
        let mut tx = self.store.begin().await?;
        tx.insert_repository(&created, &kek).await.map_err(|e| match e {
            StoreError::Duplicate(Unique::RepositoryPrefix) => AppError::conflict(
                "repository.prefix_overlaps",
                "a repository of this provider holds that prefix, or one its prune would reach",
            ),
            StoreError::Duplicate(Unique::RepositoryName) => {
                AppError::conflict("repository.name_taken", "the name is taken here")
            }
            StoreError::Duplicate(Unique::RepositoryStreams) => AppError::conflict(
                "repository.streams_taken",
                "another repository takes the streams of this organization or project",
            ),
            other => other.into(),
        })?;
        let effect = Effect::new(caller.principal.clone(), "repository.create", Outcome::Succeeded)
            .under(permit.grant)
            .after(details([
                ("repository", created.id.to_string().into()),
                ("organization", created.organization.to_string().into()),
                ("name", created.name.as_str().into()),
                ("provider", created.provider.as_str().into()),
                ("custody", created.custody.as_str().into()),
                ("streams", created.streams.into()),
            ]));
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, OPERATION, created).await
    }

    /// One repository the caller may read.
    pub async fn get_repository(&self, caller: &Caller, id: Uuid) -> Result<Repository, AppError> {
        self.authorize(caller, "repository.read", ObjectRef::new(Kind::Repository, id)).await?;
        self.store.repository(id).await?.ok_or(AppError::NotFound { kind: "repository" })
    }

    /// The live repositories of an organization, or of a project and its organization, that
    /// the caller may read.
    pub async fn list_repositories(
        &self,
        caller: &Caller,
        organization: Uuid,
        project: Option<Uuid>,
        page: Page,
    ) -> Result<Paged<Repository>, AppError> {
        let found = self.store.repositories(organization, project, page).await?;
        // The cursor passes what the caller may not see, so a page of nothing readable is not
        // the last.
        let next = page.next(&found, |r| r.id);
        let objects: Vec<ObjectRef> =
            found.iter().map(|r| ObjectRef::new(Kind::Repository, r.id)).collect();
        let readable = self.permitted_each(caller, "repository.read", &objects).await?;
        let items = found.into_iter().zip(readable).filter_map(|(r, ok)| ok.then_some(r)).collect();
        Ok(Paged { items, next })
    }
}
