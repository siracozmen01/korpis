// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declared sets as objects (§3.3 of docs/01-model.md): a project's sets are read with
//! `project.read`, and a set's drift mode is project configuration, changed with `project.write`.
//! A set is created only by the first declaration through it (`declare`).

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{Clock, EffectWrite, RecipeSource, SetWrite, Store};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::tenancy::Name;
use korpis_core::workload::set::{DeclaredSet, DriftMode};
use uuid::Uuid;

const OPERATION: &str = "workload.set_declared_set_mode";

/// A change of a set's drift mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetDriftMode {
    /// The project the set is in.
    pub project: Uuid,
    /// The set's name.
    pub name: Name,
    /// The new mode.
    pub mode: DriftMode,
    /// The set's version as the caller saw it.
    pub expected_version: u64,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The sets of a project the caller may read.
    pub async fn list_declared_sets(
        &self,
        caller: &Caller,
        project: Uuid,
        page: Page,
    ) -> Result<Paged<DeclaredSet>, AppError> {
        self.authorize(caller, "project.read", ObjectRef::new(Kind::Project, project)).await?;
        let items = self.store.declared_sets(project, page).await?;
        let next = page.next(&items, |s| s.id);
        Ok(Paged { items, next })
    }

    /// Changes what a set does with drift.
    pub async fn set_drift_mode(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, DeclaredSet>,
        cmd: SetDriftMode,
    ) -> Result<Reply<DeclaredSet>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let project = ObjectRef::new(Kind::Project, cmd.project);
        let permit = self.authorize(caller, "project.write", project).await?;
        let mut set = self
            .store
            .declared_set(cmd.project, cmd.name.as_str())
            .await?
            .ok_or(AppError::NotFound { kind: "declared set" })?;
        let mut tx = self.store.begin().await?;
        if !tx.set_drift_mode(set.id, cmd.mode, cmd.expected_version).await? {
            let message = format!("the set is at version {}", set.version);
            return Err(AppError::conflict("set.moved", message));
        }
        let effect = Effect::new(caller.principal.clone(), "set.mode", Outcome::Succeeded)
            .under(permit.grant)
            .after(details([
                ("set", set.id.to_string().into()),
                ("from", set.mode.as_str().into()),
                ("to", cmd.mode.as_str().into()),
            ]));
        tx.append_effect(&effect).await?;
        set.mode = cmd.mode;
        set.version = cmd.expected_version + 1;
        finish(tx, caller, idem, OPERATION, set).await
    }
}
