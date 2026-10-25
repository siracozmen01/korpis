// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dependencies across nodes (§9.1 of docs/07-networking.md), the control plane's half: which
//! targets of a dependent are on another node of the mesh and at what address, and which
//! dependents on other nodes a workload's node admits from the mesh. Each answer joins what two
//! nodes observed, which only the control plane holds; a node outside the mesh is told of
//! neither, so nothing is routed towards a node it has no path to.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::node::mesh::Pairs;
use korpis_core::workload::Workload;
use korpis_core::workload::dependency::{
    DependencyStatus, OverlayDependent, ProjectWorkload, Reach, ResolvedDependency, TargetEndpoint,
    TargetStatus, resolve,
};
use korpis_core::workload::observation::{Observation, ObservedState};
use korpis_core::workload::spec::dependencies::DependencyTarget;
use std::collections::BTreeSet;
use uuid::Uuid;

/// The mesh as dependencies across nodes read it: its members, and which pairs of them reach
/// each other now.
#[derive(Debug, Clone, Default)]
pub(crate) struct Members {
    nodes: BTreeSet<Uuid>,
    pairs: Pairs,
}

impl Members {
    /// How many nodes are members.
    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    fn contains(&self, node: &Uuid) -> bool {
        self.nodes.contains(node)
    }
}

/// One dependency in a project: `dependent` reaches `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Edge {
    pub(crate) dependent: Uuid,
    pub(crate) target: TargetEndpoint,
}

/// Whether `t`, as `seen` on its node, is ready: it runs and its endpoint answers.
fn ready(seen: &Observation, t: &TargetEndpoint) -> bool {
    seen.state == ObservedState::Running
        && seen.endpoints.iter().any(|e| e.name == t.endpoint && e.ready)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The members of the mesh now, and the pairs that reach each other.
    pub(crate) async fn members(&self) -> Result<Members, AppError> {
        let judged = self.judged().await?;
        Ok(Members { nodes: judged.member_nodes(), pairs: judged.pairs() })
    }

    /// Where `workload` runs on another node than `node`, both in the mesh, with what that
    /// node last observed of it there; none where it runs on `node`, is not placed, or either
    /// node is outside.
    async fn elsewhere(
        &self,
        node: Uuid,
        workload: Uuid,
        members: &Members,
    ) -> Result<Option<Observation>, AppError> {
        let Some(p) = self.store.placement(workload).await? else { return Ok(None) };
        if p.node == node || !members.contains(&node) || !members.contains(&p.node) {
            return Ok(None);
        }
        Ok(self.store.observation(workload).await?.filter(|o| o.node == p.node))
    }

    /// Gives each target of `deps` that runs on another node of the mesh than `node` the
    /// address that node observed, and its readiness there while the pair reaches each other.
    pub(crate) async fn locate(
        &self,
        node: Uuid,
        deps: &mut [ResolvedDependency],
        members: &Members,
    ) -> Result<(), AppError> {
        if members.len() < 2 {
            return Ok(());
        }
        for t in deps.iter_mut().flat_map(|d| d.targets.iter_mut()) {
            if let Some(seen) = self.elsewhere(node, t.workload, members).await? {
                // A target this node does not reach now is not ready for it, whatever its own
                // node says of it.
                let reached = members.pairs.joins(node, seen.node);
                t.address = seen.address;
                t.ready = t.address.is_some() && reached && ready(&seen, t);
            }
        }
        Ok(())
    }

    /// `w`'s dependencies as `node` is told them, each target elsewhere in the mesh located, and
    /// its dependents on other nodes, which `node` admits.
    pub(crate) async fn across(
        &self,
        (node, members): (Uuid, &Members),
        w: &Workload,
        body: &korpis_core::workload::spec::IntentBody,
    ) -> Result<(Vec<ResolvedDependency>, Vec<OverlayDependent>), AppError> {
        let mut dependencies = self.resolve_dependencies((w.project, w.id), body).await?;
        self.locate(node, &mut dependencies, members).await?;
        Ok((dependencies, self.dependents(node, w, members).await?))
    }

    /// `w`'s dependencies as its status shows them: each target and how `w` reaches it, and
    /// what each names and does not reach (§5 of docs/07-networking.md). Nodes are not named:
    /// whoever reads the workload may not read them.
    pub(crate) async fn dependency_status(
        &self,
        w: &Workload,
        target: Option<&korpis_core::workload::Intent>,
    ) -> Result<Vec<DependencyStatus>, AppError> {
        let Some(body) = target.map(|t| &t.body).filter(|b| !b.dependencies.is_empty()) else {
            return Ok(Vec::new());
        };
        let resolved = self.resolve_dependencies((w.project, w.id), body).await?;
        let own = self.store.placement(w.id).await?.map(|p| p.node);
        let pairs = self.judged().await?.pairs();
        let mut out = Vec::with_capacity(resolved.len());
        for d in resolved {
            let mut targets = Vec::with_capacity(d.targets.len());
            for t in d.targets {
                let there = self.store.placement(t.workload).await?.map(|p| p.node);
                let reach = match (own, there) {
                    (None, _) => Reach::DependentUnplaced,
                    (_, None) => Reach::TargetUnplaced,
                    (Some(a), Some(b)) if a == b => Reach::SameNode,
                    (Some(a), Some(b)) if pairs.joins(a, b) => Reach::Mesh,
                    _ => Reach::Unreached,
                };
                targets.push(TargetStatus { workload: t.workload, endpoint: t.endpoint, reach });
            }
            let (name, required, left_out) = (d.name, d.required, d.left_out);
            out.push(DependencyStatus { name, required, targets, left_out });
        }
        Ok(out)
    }

    /// Every dependency of `project`'s workloads, each to one endpoint it reaches now.
    pub(crate) async fn edges(&self, project: Uuid) -> Result<Vec<Edge>, AppError> {
        let held = self.project_workloads(project, Uuid::nil()).await?;
        let read: Vec<ProjectWorkload<'_>> = held
            .iter()
            .map(|(w, i)| ProjectWorkload {
                id: w.id,
                name: w.name.as_str(),
                labels: w.labels.as_map(),
                body: i.as_ref().map(|i| &i.body),
            })
            .collect();
        let mut out = Vec::new();
        for dependent in &read {
            let Some(body) = dependent.body else { continue };
            let others: Vec<ProjectWorkload<'_>> =
                read.iter().filter(|w| w.id != dependent.id).copied().collect();
            for d in &body.dependencies {
                let endpoint = match d.target {
                    DependencyTarget::Endpoint(id) => self.store.endpoint(id).await?,
                    _ => None,
                };
                let resolved = resolve(d, &others, endpoint.as_ref());
                out.extend(
                    resolved
                        .targets
                        .into_iter()
                        .map(|target| Edge { dependent: dependent.id, target }),
                );
            }
        }
        Ok(out)
    }

    /// The dependents of `w` on other nodes of the mesh than `node`, each with the address its
    /// node observed and the endpoint of `w` it reaches, at `w`'s address on `node`.
    pub(crate) async fn dependents(
        &self,
        node: Uuid,
        w: &Workload,
        members: &Members,
    ) -> Result<Vec<OverlayDependent>, AppError> {
        if members.len() < 2 || !members.contains(&node) {
            return Ok(Vec::new());
        }
        let own = self.store.observation(w.id).await?.filter(|o| o.node == node);
        let Some(target_address) = own.and_then(|o| o.address) else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for e in self.edges(w.project).await?.into_iter().filter(|e| e.target.workload == w.id) {
            let seen = self.elsewhere(node, e.dependent, members).await?;
            if let Some(address) = seen.and_then(|o| o.address) {
                out.push(OverlayDependent {
                    workload: e.dependent,
                    address,
                    target_address,
                    container_port: e.target.container_port,
                    protocol: e.target.protocol,
                });
            }
        }
        out.sort_by_key(|d| (d.workload, d.container_port));
        out.dedup();
        Ok(out)
    }

    /// The workloads whose nodes a change to `w` concerns besides its own, through
    /// dependencies across nodes: the workloads it depends on, and those that depend on it.
    pub(crate) async fn related(&self, w: &Workload) -> Result<Vec<Uuid>, AppError> {
        let mut out: Vec<Uuid> = self
            .edges(w.project)
            .await?
            .into_iter()
            .filter_map(|e| {
                (e.dependent == w.id)
                    .then_some(e.target.workload)
                    .or_else(|| (e.target.workload == w.id).then_some(e.dependent))
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use korpis_core::workload::observation::{EndpointSeen, Health, Usage};
    use korpis_core::workload::spec::endpoints::Protocol;

    #[test]
    fn a_target_elsewhere_is_ready_when_it_runs_and_its_endpoint_answers() {
        let target = TargetEndpoint {
            workload: Uuid::from_u128(1),
            endpoint: "pg".into(),
            container_port: 5432,
            protocol: Protocol::Tcp,
            address: None,
            ready: false,
        };
        let mut seen = Observation {
            workload: Uuid::from_u128(1),
            node: Uuid::from_u128(2),
            observed_at: std::time::UNIX_EPOCH,
            assignment_seen: 1,
            intent_seen: None,
            power_seen: 0,
            cycle_seen: 0,
            plan_progress: None,
            state: ObservedState::Running,
            reason: None,
            reason_detail: String::new(),
            health: Health::Unknown,
            exit_code: None,
            runtime_id: String::new(),
            usage: Usage::default(),
            endpoints: Vec::new(),
            volumes: Vec::new(),
            since: None,
            address: Some([10, 48, 0, 2].into()),
        };
        assert!(!ready(&seen, &target), "no endpoint seen");
        seen.endpoints.push(EndpointSeen {
            name: "pg".into(),
            address: "10.48.0.2:5432".into(),
            ready: true,
        });
        assert!(ready(&seen, &target));
        seen.state = ObservedState::Stopped;
        assert!(!ready(&seen, &target));
    }
}
