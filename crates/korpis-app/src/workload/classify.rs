// SPDX-License-Identifier: AGPL-3.0-or-later

//! The operator's layer of the classification chain (§3.3 of docs/01-model.md): per workload,
//! which plan steps are disruptive, set on the record. It may make a step more or less
//! disruptive than the driver or the recipe said, never less than the vocabulary's floor, and
//! every change is an effect naming its author, its reason, and the set before and after. Plans
//! computed after it read the new set; a plan already proposed keeps the classification its
//! proposer saw, because its approval kind is what they were shown.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, RecipeSource, Store, WorkloadWrite};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::plan::{Override, OverrideRefusal};
use korpis_core::workload::{PowerRecord, Workload};
use serde_json::{Map, Value};
use uuid::Uuid;

const OPERATION: &str = "workload.set_classification";

/// The longest reason, in characters.
const MAX_REASON: usize = 500;

/// Replacing a workload's classification overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetClassification {
    /// The workload.
    pub workload: Uuid,
    /// Its version, as the caller last read it.
    pub expected_version: u64,
    /// The whole set; empty clears it.
    pub overrides: Vec<Override>,
    /// Why, for the record.
    pub reason: String,
}

/// A workload with its new classification, and its current power, which its wire form carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    /// The workload, its version moved on.
    pub workload: Workload,
    /// Its power head.
    pub power: PowerRecord,
}

fn refused(r: OverrideRefusal) -> AppError {
    match r {
        OverrideRefusal::Duplicate(a) => {
            AppError::invalid("overrides", format!("{} is overridden twice", a.as_str()))
        }
        OverrideRefusal::BelowFloor(a) => AppError::invalid(
            "overrides",
            format!("{} is disruptive by the vocabulary, which no override relaxes", a.as_str()),
        ),
    }
}

/// The set as an effect records it: action to disruptive.
fn recorded(set: &[Override]) -> Value {
    let map: Map<String, Value> =
        set.iter().map(|o| (o.action.as_str().to_owned(), Value::Bool(o.disruptive))).collect();
    Value::Object(map)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Replaces a workload's classification overrides.
    pub async fn set_classification(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Classified>,
        cmd: SetClassification,
    ) -> Result<Reply<Classified>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let reason = cmd.reason.trim();
        if reason.is_empty() || reason.chars().count() > MAX_REASON {
            return Err(AppError::invalid("reason", "why, in at most 500 characters"));
        }
        Override::check(&cmd.overrides).map_err(refused)?;
        let object = ObjectRef::new(Kind::Workload, cmd.workload);
        let permit = self.authorize(caller, "workload.classify", object).await?;
        let mut tx = self.store.begin().await?;
        let w = tx
            .lock_workload(cmd.workload)
            .await?
            .filter(|w| w.deleted_at.is_none())
            .ok_or(AppError::NotFound { kind: "workload" })?;
        if !tx.set_classification(w.id, &cmd.overrides, cmd.expected_version).await? {
            return Err(AppError::conflict(
                "workload.version_moved",
                format!("the workload is at version {}, not {}", w.version, cmd.expected_version),
            ));
        }
        let mut e = caller
            .effect("workload.classify", Outcome::Succeeded)
            .under(permit.grant)
            .after(details([
                ("before", recorded(&w.classification)),
                ("after", recorded(&cmd.overrides)),
                ("reason", reason.into()),
            ]));
        e.workload = Some(w.id);
        tx.append_effect(&e).await?;
        let power = self
            .store
            .power(w.id)
            .await?
            .ok_or_else(|| AppError::Internal(format!("workload {} has no power", w.id)))?;
        let workload =
            Workload { classification: cmd.overrides, version: w.version.saturating_add(1), ..w };
        finish(tx, caller, idem, OPERATION, Classified { workload, power }).await
    }
}
