// SPDX-License-Identifier: AGPL-3.0-or-later

//! The transaction of a declaration: the writes, their compare-and-swaps, and the effects that
//! record them.

use super::{Base, Declared};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store, StoreError, Unique, WorkloadWrite};
use crate::workload::propose;
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::recipe::resolve::Change;
use korpis_core::tenancy::quota::Held;
use korpis_core::workload::{Plan, Workload};
use serde_json::Value;

/// The conflict a declaration against a head that has moved receives (§3.2 of
/// docs/03-state.md): the caller recomputes against the intent that won.
pub(super) fn moved(w: &Workload) -> AppError {
    AppError::conflict(
        "workload.intent_moved",
        format!("the workload's head moved: intent {} is version {}", w.intent, w.intent_version),
    )
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Admits the declaration against quota, then creates the workload, or appends the intent
    /// if neither pointer moved since the plan was computed; then inserts the plan.
    pub(super) async fn write_declaration(
        &self,
        tx: &mut S::Tx,
        base: &Base,
        d: &Declared,
        held: Option<&Held>,
        holds: &Held,
    ) -> Result<(), AppError> {
        if base.head.is_none() {
            self.admit(tx, &d.workload, held, holds).await?;
            tx.create_workload(&d.workload, &d.intent, &d.power).await.map_err(|e| match e {
                StoreError::Duplicate(Unique::WorkloadName) => AppError::conflict(
                    "workload.name_taken",
                    "a workload of that name exists in the project",
                ),
                other => other.into(),
            })?;
        } else {
            let now = tx
                .lock_workload(d.workload.id)
                .await?
                .filter(|w| w.deleted_at.is_none())
                .ok_or(AppError::NotFound { kind: "workload" })?;
            // The plan was computed from the target; if it moved, the plan is for a state that
            // no longer exists (§3.3 of docs/01-model.md).
            let stale = now.target != base.workload.target
                || now.intent_version != base.workload.intent_version;
            if stale {
                return Err(moved(&now));
            }
            // After the workload's lock, so admissions of one workload are serialized and the
            // lock order is always workload, then quota.
            self.admit(tx, &d.workload, held, holds).await?;
            if !tx.append_intent(&d.intent).await? {
                return Err(moved(&now));
            }
        }
        tx.insert_plan(&d.plan).await?;
        Ok(())
    }

    /// Approves the plan if nothing in it needs a human, and moves the target to the intent.
    pub(super) async fn approve_declared(
        &self,
        tx: &mut S::Tx,
        caller: &Caller,
        base: &Base,
        d: &mut Declared,
    ) -> Result<bool, AppError> {
        let from = d.plan.status;
        if !propose::approve_if_auto(tx, &mut d.plan, caller, self.clock.now()).await? {
            return Ok(false);
        }
        let expected = base.workload.target_version;
        if !tx.update_plan(&d.plan, from).await?
            || !tx.move_target(d.workload.id, d.intent.id, expected).await?
        {
            return Err(moved(&d.workload));
        }
        d.workload.target = Some(d.intent.id);
        d.workload.target_version = expected + 1;
        Ok(true)
    }
}

/// The effect of the declaration itself. Configuration is named by key only: a secret's value
/// never reaches the log (§5 of docs/09-recipes.md), and the intent holds the values.
pub(super) fn declaration_effect(
    caller: &Caller,
    base: &Base,
    d: &Declared,
    changes: &[Change],
    superseded: u64,
) -> Effect {
    let keys: Vec<Value> = changes.iter().map(|c| c.key.clone().into()).collect();
    let mut e = Effect::new(caller.principal.clone(), base.action, Outcome::Succeeded)
        .under(base.permit.grant)
        .after(details([
            ("intent", d.intent.id.to_string().into()),
            ("version", d.intent.version.into()),
            ("recipe", d.intent.body.recipe.digest.clone().into()),
            ("config_changed", Value::Array(keys)),
            ("approval", approval_name(&d.plan).into()),
            ("superseded_plans", superseded.into()),
        ]));
    e.workload = Some(d.workload.id);
    e.plan = Some(d.plan.id);
    e
}

/// The effect of approving a plan by default policy.
pub(super) fn approval_effect(caller: &Caller, base: &Base, plan: &Plan) -> Effect {
    let mut e = Effect::new(caller.principal.clone(), "plan.approve", Outcome::Succeeded)
        .under(base.permit.grant)
        .after(details([
            ("approval", "auto".into()),
            ("status", plan.status.as_str().into()),
            ("reason", plan.approval_reason.clone().into()),
        ]));
    e.workload = Some(plan.workload);
    e.plan = Some(plan.id);
    e
}

fn approval_name(p: &Plan) -> &'static str {
    use korpis_core::workload::plan::Approval;
    match p.approval {
        Approval::Auto => "auto",
        Approval::Confirmation => "confirmation",
        Approval::Authority { .. } => "authority",
    }
}
