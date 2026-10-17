// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading installations (§4 of docs/16-extensions.md), under `extension.read` where each is
//! installed. What an installation declares is read back from its artifact's document, which the
//! store keeps by digest, and what vouched for the artifact from its workload's intent, so neither
//! is a second copy that could disagree with the first.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::{Page, Paged};
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::extension::{ExtensionManifest, Installation};
use korpis_core::recipe::RecipeRef;
use korpis_core::workload::observation::ObservedState;
use uuid::Uuid;

/// How an installation is doing, as its workload is observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionStatus {
    /// Its workload runs.
    Running,
    /// Its workload is not running and is not failing: stopped, starting, or not yet placed.
    Stopped,
    /// Its workload crashed, or its node cannot run it.
    Failed,
}

/// An installation, with what it declares and holds.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionView {
    /// The installation.
    pub installation: Installation,
    /// What its document declares.
    pub manifest: ExtensionManifest,
    /// Its artifact as its workload's intent records it, with what vouched for it.
    pub artifact: RecipeRef,
    /// The grants it holds, one per approved need; none, once uninstalled, since uninstalling
    /// revokes them.
    pub grants: Vec<Uuid>,
    /// How it is doing.
    pub status: ExtensionStatus,
}

fn status(state: Option<ObservedState>) -> ExtensionStatus {
    match state {
        Some(ObservedState::Running) => ExtensionStatus::Running,
        Some(ObservedState::Crashed | ObservedState::Unsatisfiable) => ExtensionStatus::Failed,
        _ => ExtensionStatus::Stopped,
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// One installation, which may have been uninstalled.
    pub async fn get_extension(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<ExtensionView, AppError> {
        let installation =
            self.store.extension(id).await?.ok_or(AppError::NotFound { kind: "extension" })?;
        let target = match installation.project {
            Some(p) => ObjectRef::new(Kind::Project, p),
            None => ObjectRef::new(Kind::Organization, installation.organization),
        };
        self.authorize(caller, "extension.read", target).await?;
        self.extension_view(installation).await
    }

    /// The extensions installed in an organization.
    pub async fn list_extensions(
        &self,
        caller: &Caller,
        organization: Uuid,
        page: Page,
    ) -> Result<Paged<ExtensionView>, AppError> {
        let object = ObjectRef::new(Kind::Organization, organization);
        self.authorize(caller, "extension.read", object).await?;
        let found = self.store.extensions(organization, page).await?;
        let next = page.next(&found, |e| e.id);
        let mut items = Vec::with_capacity(found.len());
        for installation in found {
            items.push(self.extension_view(installation).await?);
        }
        Ok(Paged { items, next })
    }

    /// The manifest `installation`'s artifact declares, read from the kept artifact.
    pub(crate) async fn manifest_of(
        &self,
        installation: &Installation,
    ) -> Result<ExtensionManifest, AppError> {
        let missing =
            |what: &str| AppError::Internal(format!("extension {}: {what}", installation.id));
        let cached = self.store.cached_recipe(&installation.artifact_digest).await?;
        let cached = cached.ok_or_else(|| missing("its artifact is not kept"))?;
        let described = self
            .recipes
            .parse(&cached.document, &cached.catalogues)
            .map_err(|e| missing(&e.to_string()))?;
        described.extension.ok_or_else(|| missing("its artifact is a recipe"))
    }

    async fn extension_view(&self, installation: Installation) -> Result<ExtensionView, AppError> {
        let missing =
            |what: &str| AppError::Internal(format!("extension {}: {what}", installation.id));
        let manifest = self.manifest_of(&installation).await?;
        let workload = self.store.workload(installation.workload).await?;
        let workload = workload.ok_or_else(|| missing("its workload is gone"))?;
        let intent = self.store.intent(workload.intent).await?;
        let artifact = intent.ok_or_else(|| missing("its workload has no intent"))?.body.recipe;
        let key = installation.principal().key();
        let held = self.store.grants_held(&key, Page::new(None, Page::MAX)).await?;
        let observed = self.store.observation(installation.workload).await?;
        Ok(ExtensionView {
            status: status(observed.map(|o| o.state)),
            grants: held
                .into_iter()
                .filter(|g| g.revoked_at.is_none())
                .map(|g| g.grant.id)
                .collect(),
            artifact,
            manifest,
            installation,
        })
    }
}
