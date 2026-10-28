// SPDX-License-Identifier: AGPL-3.0-or-later

//! Proposing a migration (§8.1 of docs/05-scheduling.md): moving a placed workload to another
//! node is a plan of kind `migrate`, whose one step the control plane executes through the
//! migration's seven phases. The plan names the destination, chosen by the scheduler among every
//! node but the workload's own or named by an operator who may place there, each volume with
//! what crosses for it, how long the source keeps its copies, and every address that changes. A
//! person approves it, never a policy, since it stops the workload and holds a second node.

use super::propose::{self, Proposal};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite as _, RecipeSource, Store, WorkloadWrite as _};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::migration::{DEFAULT_HOLD, LONGEST_HOLD};
use korpis_core::workload::plan::compute::{Moving, migrate_steps};
use korpis_core::workload::plan::impact::delta;
use korpis_core::workload::plan::{Approval, Proposing};
use korpis_core::workload::spec::endpoints::Exposure;
use korpis_core::workload::{Intent, Plan, PlanTarget, Workload};
use std::time::Duration;
use uuid::Uuid;

mod begin;
mod choose;
mod conclude;
mod crossing;
mod cutover;
mod end;
mod progress;

pub use end::{EndMigration, MigrationView};

const OPERATION: &str = "workload.migrate_workload";

/// A migration to propose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateWorkload {
    /// The workload.
    pub workload: Uuid,
    /// The node to move it to; `None` for the scheduler's choice.
    pub node: Option<Uuid>,
    /// How long the source keeps its copies after cutover; `None` for a day.
    pub hold: Option<Duration>,
}

/// The refusal for anything a live migration of a workload does not allow.
pub(super) fn migrating() -> AppError {
    AppError::conflict("workload.migrating", "the workload is migrating; abandon it first")
}

/// The endpoints of `target` whose address changes when it moves, where `edge` says whether an
/// edge serves its `stable` ones: each on a port of its node.
fn changed_addresses(target: &Intent, edge: bool) -> Vec<String> {
    target
        .body
        .endpoints
        .iter()
        .filter(|e| matches!(e.exposure.resolved(edge), Exposure::NodePort | Exposure::NodeBound))
        .map(|e| e.name.clone())
        .collect()
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Proposes moving a workload to another node.
    pub async fn migrate_workload(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        cmd: MigrateWorkload,
    ) -> Result<Reply<Plan>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let hold = cmd.hold.unwrap_or(DEFAULT_HOLD);
        if hold.is_zero() || hold > LONGEST_HOLD {
            return Err(AppError::invalid("hold", "more than nothing, and at most 31 days"));
        }
        let object = ObjectRef::new(Kind::Workload, cmd.workload);
        let (permit, proposing) =
            self.authorize_declaration(caller, "workload.migrate", object).await?;
        if let Some(node) = cmd.node {
            self.authorize(caller, "node.place", ObjectRef::new(Kind::Node, node)).await?;
        }
        let w = self.live_workload(cmd.workload).await?;
        let target = self.approved_target(&w).await?;
        self.migratable(&w).await?;
        let mut tx = self.store.begin().await?;
        let toward = cmd.node.map_or(choose::Toward::Any, choose::Toward::Named);
        let chosen = self.destination(&mut tx, (&w, &target), toward).await?;
        let volumes = self.crossings(&w, (&chosen.source, &chosen.node)).await?;
        let moving = Moving {
            source: chosen.source.id,
            destination: chosen.node.id,
            named: cmd.node.is_some(),
            hold,
            volumes: volumes.iter().map(crossing::crossed).collect(),
            live: false,
            changed_addresses: changed_addresses(&target, chosen.edge),
        };
        let plan = self.migrate_plan(caller, &w, (&proposing, permit.grant), &moving).await?;
        let locked = tx.lock_workload(w.id).await?.filter(|l| l.deleted_at.is_none());
        if locked.is_none_or(|l| l.target != w.target) {
            return Err(AppError::conflict("workload.moved", "the workload changed meanwhile"));
        }
        tx.insert_plan(&plan).await?;
        let mut e = caller
            .effect("workload.migrate", Outcome::Succeeded)
            .under(permit.grant)
            .after(details([
                ("source", moving.source.to_string().into()),
                ("destination", moving.destination.to_string().into()),
                ("hold_seconds", moving.hold.as_secs().into()),
            ]));
        (e.workload, e.plan) = (Some(w.id), Some(plan.id));
        tx.append_effect(&e).await?;
        finish(tx, caller, idem, OPERATION, plan).await
    }

    /// Whether `w` may be moved at all: it is placed, not migrating already, not being retired,
    /// and holds no address of a `node` pool, which is routed to its node alone (§3.7 of
    /// docs/07-networking.md).
    async fn migratable(&self, w: &Workload) -> Result<(), AppError> {
        if self.store.placement(w.id).await?.is_none() {
            return Err(AppError::conflict("workload.unplaced", "the workload is on no node"));
        }
        if self.store.migrations_of(w.id).await?.iter().any(|(m, _)| m.live()) {
            return Err(migrating());
        }
        if self.store.dedicated_of(w.id).await?.is_some_and(|h| !h.cluster) {
            let why = "it holds an address of a node pool, which is routed to its node alone";
            return Err(AppError::conflict("address.node_pool", why));
        }
        Ok(())
    }

    /// The plan moving `w` as `moving` says, proposed by `caller` under `grant`; approved by a
    /// person whatever the policy would allow.
    async fn migrate_plan(
        &self,
        caller: &Caller,
        w: &Workload,
        (proposing, grant): (&Proposing, Uuid),
        moving: &Moving,
    ) -> Result<Plan, AppError> {
        let (driver, disruption, wait) = self.running_layers(w).await?;
        let classifier = propose::classifier(&driver, (&disruption, wait), w);
        let steps = migrate_steps(w.id, moving, classifier);
        let proposal = Proposal {
            exposures: Vec::new(),
            workload: w.id,
            target: PlanTarget::Migrate,
            from_intent: w.target,
            from_power: w.power_target_version,
            delta: delta(None, None),
            steps,
            proposing,
            grant,
        };
        let mut plan = propose::plan(caller, proposal, self.clock.now());
        if plan.approval == Approval::Auto {
            plan.approval = Approval::Confirmation;
            plan.approval_reason = "a migration stops its workload; a person approves it".into();
        }
        Ok(plan)
    }
}
