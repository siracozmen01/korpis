// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a declaration is made against: the workload as it stands or as it will be created, and
//! the body completed from its recipe. An extension's artifact is installed, never declared
//! (§4 of docs/16-extensions.md): a plain workload may not run one, and an extension's workload
//! runs only the digest it was installed from, so what its grants were approved for is what runs.

use super::{Base, NewWorkload, moved};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, Described, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::recipe::resolve::Change;
use korpis_core::recipe::{Recipe, RecipeRef};
use korpis_core::workload::declare::Declaration;
use korpis_core::workload::spec::IntentBody;
use korpis_core::workload::{Power, PowerRecord, Workload};
use uuid::Uuid;

/// Refuses an artifact whose kind the workload may not run.
fn same_kind(base: &Base, reference: &RecipeRef, described: &Described) -> Result<(), AppError> {
    let why = match (&base.extension, described.extension.is_some()) {
        (None, false) => return Ok(()),
        (Some(installed), true) if *installed == reference.digest => return Ok(()),
        (None, true) => "an extension is installed, not declared (§4 of docs/16-extensions.md)",
        (Some(_), false) => "an extension's workload runs only the extension it was installed from",
        (Some(_), true) => "another version of an extension is installed, not declared",
    };
    Err(AppError::invalid("recipe", why))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The declared body completed from its recipe, resolved under the organization's trust
    /// policy, with its configuration resolved and what that changed.
    pub(super) async fn complete_declaration(
        &self,
        caller: &Caller,
        base: &Base,
        (name, version): (&str, &str),
        declaration: Declaration,
    ) -> Result<(IntentBody, Recipe, Vec<Change>), AppError> {
        let organization = base.workload.organization;
        let (reference, described) = self.resolve_trusted(name, version, organization).await?;
        same_kind(base, &reference, &described)?;
        let recipe = described.recipe;
        let mut body = declaration
            .complete(reference, &recipe)
            .map_err(|e| AppError::invalid(format!("body.{}", e.field), e.message))?;
        super::enforced::refuse_unenforced(&body)?;
        if let Some(zone) = &body.timezone
            && self.zones.zone(zone).is_none()
        {
            return Err(AppError::invalid(
                "body.timezone",
                "a zone the control plane does not know",
            ));
        }
        body.recipe.providers = self.pin_providers(&recipe, &body.health, organization).await?;
        self.check_dependencies((base.workload.project, base.workload.id), &body).await?;
        let previous = base.head.as_ref().map(|h| &h.body.config);
        let changes = self.configure(caller, &mut body, &recipe, previous, organization).await?;
        Ok((body, recipe, changes))
    }

    pub(super) async fn base_new(
        &self,
        caller: &Caller,
        n: &NewWorkload,
    ) -> Result<Base, AppError> {
        let object = ObjectRef::new(Kind::Project, n.project);
        let (permit, proposing) =
            self.authorize_declaration(caller, "workload.create", object).await?;
        let project =
            self.store.project(n.project).await?.ok_or(AppError::NotFound { kind: "project" })?;
        let now = self.clock.now();
        let id = Uuid::now_v7();
        let workload = Workload {
            id,
            project: project.id,
            organization: project.organization,
            name: n.name.clone(),
            labels: n.labels.clone(),
            intent: Uuid::nil(),
            intent_version: 0,
            target: None,
            target_version: 0,
            power_version: 1,
            power_target_version: 0,
            version: 1,
            created: now,
            deleted_at: None,
            set: None,
            classification: Vec::new(),
            unreproducible: None,
        };
        let power = PowerRecord {
            workload: id,
            power: Power::initial(n.power),
            set_by: caller.principal.clone(),
            set_at: now,
        };
        let action = "workload.create";
        let (head, target, extension) = (None, None, None);
        let applied = None;
        Ok(Base { workload, power, head, target, applied, permit, proposing, action, extension })
    }

    pub(super) async fn base_existing(
        &self,
        caller: &Caller,
        id: Uuid,
        expected: u64,
    ) -> Result<Base, AppError> {
        let object = ObjectRef::new(Kind::Workload, id);
        let (permit, proposing) =
            self.authorize_declaration(caller, "workload.write", object).await?;
        let workload = self
            .store
            .workload(id)
            .await?
            .filter(|w| w.deleted_at.is_none())
            .ok_or(AppError::NotFound { kind: "workload" })?;
        if workload.intent_version != expected {
            return Err(moved(&workload));
        }
        let intent = |i: Uuid| async move {
            self.store.intent(i).await?.ok_or_else(|| AppError::Internal(format!("intent {i}")))
        };
        let head = Some(intent(workload.intent).await?);
        let target = match workload.target {
            Some(t) => Some(intent(t).await?),
            None => None,
        };
        let applied = self.reached_intent(id, target.clone()).await?;
        let power = self
            .store
            .power(id)
            .await?
            .ok_or_else(|| AppError::Internal(format!("workload {id} has no power")))?;
        let action = "workload.write";
        let extension = self.store.extension_of_workload(id).await?.map(|e| e.artifact_digest);
        Ok(Base { workload, power, head, target, applied, permit, proposing, action, extension })
    }
}
