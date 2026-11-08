// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the panel shows in a slot: the contributions of the installations that cover an object,
//! each only if the viewer may invoke its action there, so a slot never offers what its viewer
//! cannot do. A workload is covered by what is installed in its project and in its organization
//! and every organization above; an organization's settings by what is installed into it.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::extension::slot::{self, ORGANIZATION_SETTINGS, WORKLOAD_PANEL, Widget};
use korpis_core::extension::{ActionDecl, Installation};
use uuid::Uuid;

/// One installation's contribution to a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotShown {
    /// The installation.
    pub installation: Installation,
    /// How it is shown.
    pub widget: Widget,
    /// Its heading.
    pub title: String,
    /// The action it shows or invokes.
    pub action: ActionDecl,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The contributions to `slot` shown for `object`, a workload for [`WORKLOAD_PANEL`] and an
    /// organization for [`ORGANIZATION_SETTINGS`], that the caller may read and invoke.
    ///
    /// # Errors
    /// [`AppError::Invalid`] for a slot the panel does not offer or an object of the wrong kind.
    pub async fn list_slots(
        &self,
        caller: &Caller,
        slot: &str,
        object: ObjectRef,
    ) -> Result<Vec<SlotShown>, AppError> {
        let covering = match (slot, object.kind) {
            (WORKLOAD_PANEL, Kind::Workload) => {
                self.authorize(caller, "workload.read", object).await?;
                self.covering_workload(object.id).await?
            }
            (ORGANIZATION_SETTINGS, Kind::Organization) => {
                self.authorize(caller, "org.read", object).await?;
                self.installed_in(object.id)
                    .await?
                    .into_iter()
                    .filter(|i| i.project.is_none())
                    .collect()
            }
            _ => {
                let why =
                    format!("one of {}, for a workload or an organization", slot::SLOTS.join(", "));
                return Err(AppError::invalid("slot", why));
            }
        };
        let mut shown = Vec::new();
        for installation in covering {
            let manifest = self.manifest_of(&installation).await?;
            let Some(decl) = manifest.ui_slots.iter().find(|s| s.slot == slot) else { continue };
            let Ok(c) = slot::read(slot, &decl.descriptor, &manifest.actions) else { continue };
            let Some(action) = manifest.actions.iter().find(|a| a.name == c.action) else {
                continue;
            };
            if !self.may_invoke(caller, &installation, &action.name, object).await? {
                continue;
            }
            shown.push(SlotShown {
                installation,
                widget: c.widget,
                title: c.title,
                action: action.clone(),
            });
        }
        Ok(shown)
    }

    /// Every installation in `organization`, installed now.
    async fn installed_in(&self, organization: Uuid) -> Result<Vec<Installation>, AppError> {
        let mut all = Vec::new();
        let mut page = Page::new(None, Page::MAX);
        loop {
            let found = self.store.extensions(organization, page).await?;
            let next = page.next(&found, |e| e.id);
            all.extend(found.into_iter().filter(Installation::is_installed));
            match next {
                Some(after) => page = Page::new(Some(after), Page::MAX),
                None => return Ok(all),
            }
        }
    }

    /// The installations covering workload `id`: in its project, and in its organization and
    /// every organization above, installed into the organization as a whole.
    async fn covering_workload(&self, id: Uuid) -> Result<Vec<Installation>, AppError> {
        let workload =
            self.store.workload(id).await?.ok_or(AppError::NotFound { kind: "workload" })?;
        let project = self
            .store
            .project(workload.project)
            .await?
            .ok_or(AppError::NotFound { kind: "project" })?;
        let mut covering = Vec::new();
        for (depth, org) in
            self.organization_chain(project.organization).await?.into_iter().enumerate()
        {
            // Installed into the organization as a whole, or into the workload's own project.
            let own = |i: &Installation| {
                i.project.is_none() || (depth == 0 && i.project == Some(project.id))
            };
            covering.extend(self.installed_in(org).await?.into_iter().filter(own));
        }
        Ok(covering)
    }
}
