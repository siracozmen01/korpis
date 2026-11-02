// SPDX-License-Identifier: AGPL-3.0-or-later

//! Settling an item under way against what is known of it. A drained workload applies until its
//! migration has moved it, and fails when the migration ends abandoned or rolled back; an evacuated
//! one applies until its remake plan has applied, and fails when the plan does. Either then settles
//! until the node it now runs on reports it running, or stopped as its power says, and counts as
//! unavailable until then: a workload that fails to start where it lands holds its operation's
//! budget, which is what `max_unavailable` is for.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::operation::{Item, ItemState, Kind, Operation, Scope};
use korpis_core::workload::migration::Outcome;
use korpis_core::workload::observation::ObservedState;
use korpis_core::workload::plan::PlanStatus;
use std::time::SystemTime;
use uuid::Uuid;

/// How far an item's plan has carried its workload.
enum Carried {
    /// Not yet off the node.
    Not,
    /// Off the node.
    Off,
    /// Never will be, for the reason given.
    Failed(String),
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Moves `item`, under way in `op`, as far as what is known of it at `now` allows.
    pub(super) async fn settle(
        &self,
        op: &Operation,
        item: &mut Item,
        now: SystemTime,
    ) -> Result<(), AppError> {
        let Scope::Node(left) = op.scope else { return Ok(()) };
        let live = self.store.workload(item.subject).await?.filter(|w| w.deleted_at.is_none());
        if live.is_none() {
            item.end(ItemState::Skipped, "the workload was deleted meanwhile", now);
            return Ok(());
        }
        let carried = match (op.kind, item.plan) {
            // Placed again with nothing to make: it settles where it landed.
            (_, None) => Carried::Off,
            (Kind::Drain, Some(plan)) => self.migrated(item.subject, plan).await?,
            (Kind::Evacuate | Kind::TemplateRollout, Some(plan)) => self.remade(plan).await?,
        };
        match carried {
            Carried::Failed(why) => item.end(ItemState::Failed, &why, now),
            Carried::Not => {}
            Carried::Off => {
                item.state = ItemState::Settling;
                if self.seen_where_placed(item.subject, left).await? {
                    item.end(ItemState::Done, "", now);
                }
            }
        }
        Ok(())
    }

    /// How far the migration plan `plan` has moved `workload`.
    async fn migrated(&self, workload: Uuid, plan: Uuid) -> Result<Carried, AppError> {
        let migrations = self.store.migrations_of(workload).await?;
        let Some((m, _)) = migrations.into_iter().find(|(m, _)| m.plan == plan) else {
            return self.remade(plan).await.map(|c| match c {
                // A migration plan carries nothing without its migration.
                Carried::Off => Carried::Failed("its plan applied without a migration".into()),
                other => other,
            });
        };
        Ok(match m.outcome {
            Some((Outcome::Abandoned | Outcome::RolledBack, why)) => Carried::Failed(why),
            Some((Outcome::Completed, _)) => Carried::Off,
            None if m.moved_at.is_some() => Carried::Off,
            None => Carried::Not,
        })
    }

    /// How far the plan `plan` has gone: off once it applied.
    async fn remade(&self, plan: Uuid) -> Result<Carried, AppError> {
        let status = self.store.plan(plan).await?.map(|p| p.status);
        Ok(match status {
            Some(PlanStatus::Applied) => Carried::Off,
            Some(s) if s.is_terminal() => Carried::Failed(format!("its plan is {}", s.as_str())),
            Some(_) => Carried::Not,
            None => Carried::Failed("its plan is gone".into()),
        })
    }

    /// Whether `workload` is placed off `left` and its node there reports it running or stopped.
    async fn seen_where_placed(&self, workload: Uuid, left: Uuid) -> Result<bool, AppError> {
        let Some(at) = self.store.placement(workload).await?.map(|p| p.node) else {
            return Ok(false);
        };
        let seen = self.store.observation(workload).await?;
        Ok(at != left
            && seen.is_some_and(|o| {
                o.node == at && matches!(o.state, ObservedState::Running | ObservedState::Stopped)
            }))
    }
}
