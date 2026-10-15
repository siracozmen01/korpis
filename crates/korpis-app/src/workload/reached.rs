// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a workload's plans have made of it (§3.3 of docs/01-model.md). Approval moves the target
//! before anything runs, so the target is what a workload is to become, not what it is. An intent
//! whose plan failed was never reached, and the one that plan started from is looked at instead,
//! until one that was, or none: declarations compute their plans from that (Finding 51 of
//! docs/23-walkthroughs.md). A node is told less (§4.8 of docs/02-architecture.md, Finding 52
//! there): an intent or power whose plan has not begun, waiting for its window or behind another
//! plan, is not on the node either, so it is passed over too.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::workload::Intent;
use korpis_core::workload::plan::{Plan, PlanStatus};
use uuid::Uuid;

/// How many unreached plans in a row a search follows back. Past that many, the search answers
/// nothing reached, which is safe: every step is idempotent, so planning from nothing completes
/// again as no-ops what was done, and a node told nothing runs nothing.
const MAX_UNREACHED_CHAIN: usize = 64;

/// Whether `plan` failed to reach its target.
fn failed(plan: &Plan) -> bool {
    plan.status == PlanStatus::Failed
}

/// Whether `plan` has not put its target on a node: it failed, or it has not begun.
fn not_on_node(plan: &Plan) -> bool {
    matches!(plan.status, PlanStatus::Failed | PlanStatus::Approved | PlanStatus::Queued)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The last intent of `workload`'s that its plans reached, from `target` back: what a
    /// declaration's plan is computed from.
    pub(crate) async fn reached_intent(
        &self,
        workload: Uuid,
        target: Option<Intent>,
    ) -> Result<Option<Intent>, AppError> {
        self.intent_back(workload, target, failed).await
    }

    /// The last intent of `workload`'s that its plans put on its node, from `target` back: what
    /// the node is told when no plan it may act on now is reaching another.
    pub(crate) async fn intent_on_node(
        &self,
        workload: Uuid,
        target: Option<Intent>,
    ) -> Result<Option<Intent>, AppError> {
        self.intent_back(workload, target, not_on_node).await
    }

    /// Follows `target` back through the plans `missed` says did not reach theirs.
    async fn intent_back(
        &self,
        workload: Uuid,
        target: Option<Intent>,
        missed: fn(&Plan) -> bool,
    ) -> Result<Option<Intent>, AppError> {
        let mut at = target;
        for _ in 0..MAX_UNREACHED_CHAIN {
            let Some(i) = &at else { return Ok(None) };
            let plan = self.store.latest_plan_to(workload, i.id).await?;
            let Some(plan) = plan.filter(missed) else { return Ok(at) };
            at = match plan.from_intent {
                Some(f) => self.store.intent(f).await?,
                None => None,
            };
        }
        Ok(None)
    }

    /// The last power version of `workload`'s that its plans put on its node, from `version`
    /// back; zero for none. The first power declaration is approved with the first intent and has
    /// no plan of its own, so it is on the node with that intent.
    pub(crate) async fn power_on_node(
        &self,
        workload: Uuid,
        version: u64,
    ) -> Result<u64, AppError> {
        let mut at = version;
        for _ in 0..MAX_UNREACHED_CHAIN {
            if at == 0 {
                return Ok(0);
            }
            let plan = self.store.latest_plan_to_power(workload, at).await?;
            let Some(missed) = plan.filter(not_on_node) else { return Ok(at) };
            at = missed.from_power;
        }
        Ok(0)
    }
}
