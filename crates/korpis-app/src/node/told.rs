// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node is told to hold of a workload (§4.8 of docs/02-architecture.md): the intent and
//! power its plans reached, or that its active plan is reaching. Approval moves the target before
//! anything runs, and a node runs what it is told, so a node told the target ran what no plan had
//! made: a create approved for a later window started at once, without its install, and so did a
//! create whose plan failed (Finding 52 of docs/23-walkthroughs.md). A workload no plan has
//! brought to any intent is still told its target, so the node keeps what the plan's steps
//! saved, such as an install's progress, but with nothing to run.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::plan::{Plan, PlanTarget};
use korpis_core::workload::{Intent, Power, PowerRecord, PowerState, Workload};

/// The power a node is told for a workload no plan has brought to any intent: stopped, at
/// version zero, which no declaration has.
fn nothing_to_run(w: &Workload) -> PowerRecord {
    PowerRecord {
        workload: w.id,
        power: Power { desired: PowerState::Stopped, cycle: 0, version: 0 },
        set_by: Principal::System(SystemActor::Reconciler),
        set_at: w.created,
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The intent and power `w`'s node is told, with `active` its plan the node may act on now;
    /// nothing while it has no target.
    pub(super) async fn told(
        &self,
        w: &Workload,
        active: Option<&Plan>,
    ) -> Result<Option<(Intent, PowerRecord)>, AppError> {
        let Some(id) = w.target else { return Ok(None) };
        let missing = |what: &str| AppError::Internal(format!("{what} of workload {}", w.id));
        let target = self.store.intent(id).await?.ok_or_else(|| missing("target"))?;
        let reaching = active.map(|p| p.target);
        let intent = match reaching {
            Some(PlanTarget::Intent(i)) => self.store.intent(i).await?,
            _ => self.intent_on_node(w.id, Some(target.clone())).await?,
        };
        let Some(intent) = intent else { return Ok(Some((target, nothing_to_run(w)))) };
        let version = match reaching {
            Some(PlanTarget::Power(v)) => v,
            _ => self.power_on_node(w.id, w.power_target_version).await?,
        };
        if version == 0 {
            return Ok(Some((intent, nothing_to_run(w))));
        }
        let power = self.store.power_at(w.id, version).await?.ok_or_else(|| missing("power"))?;
        Ok(Some((intent, power)))
    }
}
