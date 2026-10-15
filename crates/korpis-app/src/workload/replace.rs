// SPDX-License-Identifier: AGPL-3.0-or-later

//! Re-placing a workload whose bound node's driver refused it after the bind (§2.1 of
//! docs/05-scheduling.md). The node runs `Prepare` again before it makes anything, because the
//! node may have changed since the shortlist asked, and a refusal before anything of the workload
//! was made there is journalled as `step.refused`. Nothing there is lost by moving it: the refusal
//! is kept for the target, the placement is undone, and the workload is placed again with that
//! node left out, its plan still open, to run from its first step wherever it lands. A workload an
//! operator placed by hand is not moved, since the scheduler does not overrule that choice (§2.3);
//! its plan fails as any other does, and a workload that ran on the node never reports a refusal
//! this way, so it stays and moving it is migration (§8).

use super::place::place_effect;
use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, EffectWrite, PlacementWrite, RecipeSource, Store, WorkloadWrite};
use korpis_core::audit::Outcome;
use korpis_core::node::journal::NodeEffect;
use korpis_core::placement::BoundBy;
use korpis_core::workload::{Plan, PlanTarget};
use serde_json::Value;
use uuid::Uuid;

/// The error reason a node journals for a step its driver refused before anything was made.
pub(crate) const REFUSED: &str = "step.refused";

/// Why `n` failed, when it failed because the driver refused before anything was made.
pub(crate) fn refused_before_anything(n: &NodeEffect) -> Option<String> {
    let error = n.error.as_ref()?;
    if n.outcome != Outcome::Failed || error.get("reason") != Some(&Value::from(REFUSED)) {
        return None;
    }
    let message = error.get("message").and_then(Value::as_str).unwrap_or_default();
    Some(if message.is_empty() {
        "its driver refused it after the bind".to_owned()
    } else {
        message.to_owned()
    })
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Places `plan`'s workload again after `node`'s driver refused it, for `why`, before
    /// anything of it was made there; whether it did. It does not when the workload is not
    /// placed on `node` for the plan's target, or an operator placed it there.
    pub(crate) async fn replace_refused(
        &self,
        tx: &mut S::Tx,
        node: Uuid,
        plan: &Plan,
        why: &str,
    ) -> Result<bool, AppError> {
        let PlanTarget::Intent(intent) = plan.target else { return Ok(false) };
        let Some(w) = tx.lock_workload(plan.workload).await? else { return Ok(false) };
        let Some(p) = tx.placement_in(w.id).await? else { return Ok(false) };
        let here = p.node == node && p.intent == intent && w.target == Some(intent);
        if !here || p.bound_by == BoundBy::Operator || w.deleted_at.is_some() {
            return Ok(false);
        }
        let Some(target) = self.store.intent(intent).await? else { return Ok(false) };
        tx.refuse((w.id, intent), node, why, self.clock.now()).await?;
        tx.unbind(w.id).await?;
        let grant = tx.decision_grant(w.id).await?;
        tx.append_effect(&place_effect(&w, Outcome::RolledBack, grant)).await?;
        self.place_target(tx, &w, Some(&target)).await?;
        Ok(true)
    }
}
