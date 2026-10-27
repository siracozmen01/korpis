// SPDX-License-Identifier: AGPL-3.0-or-later

//! A residual assignment (§7 of docs/06-storage.md): a workload that no longer runs on the node
//! but left a volume there is sent the plan deleting it, and nothing the node acts on besides.

use super::{Desired, active_plan};
use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::workload::assignment::{Assignment, IdRange, NodePlan};
use std::collections::BTreeMap;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The residual assignment of `workload` on `node`: the plan deleting a volume it left there,
    /// with the last target and power it had for the record. No ids, no lease, no recipe it
    /// needs; `None` once nothing is left to do there.
    pub(super) async fn residual(
        &self,
        node: Uuid,
        workload: Uuid,
    ) -> Result<Option<Desired>, AppError> {
        let Some(w) = self.store.workload(workload).await? else { return Ok(None) };
        let missing = |what: &str| AppError::Internal(format!("{what} of workload {workload}"));
        let id = w.target.ok_or_else(|| missing("target"))?;
        let target = self.store.intent(id).await?.ok_or_else(|| missing("target"))?;
        let power = self
            .store
            .power_at(workload, w.power_target_version)
            .await?
            .ok_or_else(|| missing("power target"))?;
        let mut here = Vec::new();
        for p in self.store.open_plans(workload).await? {
            if let Some(v) = p.target.volume()
                && self.store.volume(v).await?.is_some_and(|v| v.keeper() == node)
            {
                here.push(p);
            }
        }
        let Some(plan) = active_plan(here, self.clock.now()) else { return Ok(None) };
        let on_lease_expiry = self.on_expiry(node, &target.body).await?;
        Ok(Some(Desired {
            assignment: Assignment {
                workload,
                project: w.project,
                organization: w.organization,
                name: w.name.clone(),
                labels: w.labels.clone(),
                generation: 0,
                target,
                power,
                on_lease_expiry,
                plan: NodePlan::of(&plan),
                retire: None,
                recipe_manifest: Vec::new(),
                node,
                lease_epoch: 0,
                ids: IdRange { base: 0, count: 0 },
                residual: true,
                volume_ids: BTreeMap::new(),
                endpoints: Vec::new(),
                reaches_api: false,
                egress: None,
                dependencies: Vec::new(),
                challenges: Vec::new(),
                alpn_challenges: Vec::new(),
                runs: Vec::new(),
                kept_runs: Vec::new(),
                devices: Vec::new(),
                cores: Vec::new(),
                dependents: Vec::new(),
                peers: Vec::new(),
                dedicated: None,
                block_attachments: Vec::new(),
            },
            plan: Some(plan),
            held: false,
        }))
    }
}
