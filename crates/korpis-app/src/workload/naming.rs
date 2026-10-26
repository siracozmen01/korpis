// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rules naming workloads (§6 of docs/07-networking.md), the control plane's half: each rule of a
//! workload that names workloads by a selector, resolved to the addresses its node routes to the
//! workloads of its project the selector matches, as their nodes observed them. Only the control
//! plane holds where every workload runs and at what address, so it alone resolves them.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use crate::workload::Members;
use korpis_core::workload::Workload;
use korpis_core::workload::peers::{Located, Resolved, Routes, naming_workloads, resolve};
use korpis_core::workload::spec::IntentBody;
use std::net::IpAddr;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Each rule of `body`, `w`'s, that names workloads, resolved for `node`: the addresses it
    /// names there, what it matched, and what it named at no address, with why.
    pub(crate) async fn rule_peers(
        &self,
        (node, members): (Uuid, &Members),
        w: &Workload,
        body: &IntentBody,
    ) -> Result<Vec<Resolved>, AppError> {
        if naming_workloads(body).is_empty() {
            return Ok(Vec::new());
        }
        let held = self.project_workloads(w.project, w.id).await?;
        let mut located = Vec::with_capacity(held.len());
        for (other, _) in &held {
            let placed = self.store.placement(other.id).await?.map(|p| p.node);
            // Only what its node observes where it is placed now: an address it held elsewhere
            // before is another workload's, or no one's.
            let seen = self.store.observation(other.id).await?.filter(|o| Some(o.node) == placed);
            let addresses: Vec<IpAddr> = seen
                .map(|o| {
                    o.address.map(IpAddr::V4).into_iter().chain(o.address_ipv6.map(IpAddr::V6))
                })
                .into_iter()
                .flatten()
                .collect();
            located.push(Located {
                id: other.id,
                labels: other.labels.as_map(),
                node: placed,
                addresses,
            });
        }
        Ok(resolve(body, &located, Routes { node, members: &members.pools }))
    }

    /// Each rule of `target`, `w`'s, that names workloads, as its status shows it: resolved for
    /// the node `w` is placed on; none while it is not placed, since what a rule names depends
    /// on its node.
    pub(crate) async fn named_rule_status(
        &self,
        w: &Workload,
        target: Option<&korpis_core::workload::Intent>,
    ) -> Result<Vec<Resolved>, AppError> {
        let Some(body) = target.map(|t| &t.body) else { return Ok(Vec::new()) };
        if naming_workloads(body).is_empty() {
            return Ok(Vec::new());
        }
        let Some(node) = self.store.placement(w.id).await?.map(|p| p.node) else {
            return Ok(Vec::new());
        };
        let members = self.members().await?;
        self.rule_peers((node, &members), w, body).await
    }

    /// The workloads of `w`'s project, `w` aside, whose rules name workloads: their nodes are
    /// told again whenever `w` changes, since it may be one they name (§6 of
    /// docs/07-networking.md).
    pub(crate) async fn naming_in_project(&self, w: &Workload) -> Result<Vec<Uuid>, AppError> {
        let held = self.project_workloads(w.project, w.id).await?;
        let names = |i: &korpis_core::workload::Intent| !naming_workloads(&i.body).is_empty();
        Ok(held.iter().filter(|(_, i)| i.as_ref().is_some_and(names)).map(|(o, _)| o.id).collect())
    }
}
