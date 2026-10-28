// SPDX-License-Identifier: AGPL-3.0-or-later

//! A migration's destination (§8.1 of docs/05-scheduling.md): the scheduler's choice among every
//! node serving the workload but the one it is on, or a node an operator named, checked as a
//! placement by hand is (§2.3 there), its cordon and taints overridden and nothing else. The
//! volumes it must reach where they are are those of shared and block classes alone; every other
//! volume crosses to it.

use super::super::learned::Learned;
use super::super::place::{Given, candidates_of, given_on};
use super::super::placed_quota::counting;
use crate::App;
use crate::edge_group::Addressing;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, Placed, PlacementWrite as _, RecipeSource, Store};
use korpis_core::node::Node;
use korpis_core::node::device::DeviceHold;
use korpis_core::node::mesh::Pairs;
use korpis_core::schedule::policy::Policies;
use korpis_core::schedule::{
    Candidate, Explanation, Needed, Peer, Request, operator_refusals, place,
};
use korpis_core::tenancy::quota::{Bounds, Promise, QuotaEntry};
use korpis_core::workload::volume::Home;
use korpis_core::workload::{Intent, Workload};
use std::collections::BTreeMap;
use uuid::Uuid;

/// A destination, with the node the workload leaves.
pub(in crate::workload) struct Chosen {
    /// The node it is on.
    pub(in crate::workload) source: Node,
    /// The node it goes to.
    pub(in crate::workload) node: Node,
    /// Why, as the scheduler sees it.
    pub(in crate::workload) explanation: Explanation,
    /// What it is given there.
    pub(in crate::workload) given: Given,
    /// Whether an edge serves its `stable` endpoints, so their address stays.
    pub(in crate::workload) edge: bool,
}

/// What the scheduler reads of the candidates, read once.
struct Inputs {
    owners: Vec<Uuid>,
    nodes: Vec<Node>,
    allocations: BTreeMap<Uuid, Bounds>,
    held: BTreeMap<Uuid, Vec<DeviceHold>>,
    held_cores: BTreeMap<Uuid, Vec<u32>>,
    peers: Vec<Peer>,
    volumes: Vec<(Uuid, Home)>,
    needed: Vec<Needed>,
    mesh: Pairs,
    refused: BTreeMap<Uuid, String>,
    learned: Learned,
    entries: Vec<QuotaEntry>,
    placed: Option<Placed>,
    promised: Vec<Vec<Promise>>,
    addressing: Addressing,
    policies: Policies,
}

/// The refusal of a migration no node can take, naming each node's first refusal.
fn nowhere(explanation: &Explanation) -> AppError {
    let mut why: BTreeMap<Uuid, String> = BTreeMap::new();
    for r in &explanation.filtered {
        why.entry(r.node).or_insert_with(|| format!("{}: {}", r.check.as_str(), r.detail));
    }
    let named: Vec<String> = why.into_iter().map(|(n, w)| format!("{n} ({w})")).collect();
    AppError::Unsatisfiable {
        reason: "placement.unsatisfiable",
        message: format!("no other node can run it: {}", named.join("; ")),
    }
}

/// The scheduler's view of moving `w`, approved to be `target`, to `named` or the best of the
/// candidates in `i`; with what it is given there.
fn pick(
    seen: &Inputs,
    (w, target): (&Workload, &Intent),
    named: Option<Uuid>,
) -> Result<(Explanation, Given), AppError> {
    let quota = counting(&seen.entries, seen.placed.as_ref(), seen.promised.clone());
    let req = Request {
        workload: w.id,
        project: w.project,
        labels: w.labels.as_map(),
        owners: &seen.owners,
        body: &target.body,
        current: None,
        edge_tier: seen.addressing.stable(),
        routed: seen.addressing.routed,
        ingress_edge: seen.addressing.ingress.is_some(),
        edge_members: seen.addressing.edge_members(),
        volumes: &seen.volumes,
        refused: &seen.refused,
        content: &seen.learned.content,
        churn: &seen.learned.churn,
        dependencies: &seen.needed,
        mesh: &seen.mesh,
        quota: &quota,
        devices: &seen.held,
        cores: &seen.held_cores,
        dedicated: &seen.addressing.dedicated,
    };
    let empty = Bounds::default();
    let explanation =
        if let Some(node) = named.and_then(|id| seen.nodes.iter().find(|n| n.id == id)) {
            let allocated = seen.allocations.get(&node.id).unwrap_or(&empty);
            let candidate = Candidate { node, allocated };
            let standing = operator_refusals(&req, &candidate, &seen.peers, &seen.policies);
            if !standing.is_empty() {
                return Err(super::super::pin::refused(&standing));
            }
            let mut chosen = place(&req, &[candidate], &seen.peers, &seen.policies);
            chosen.chosen = Some(node.id);
            chosen
        } else {
            let candidates = candidates_of(&seen.nodes, &seen.allocations, &empty);
            place(&req, &candidates, &seen.peers, &seen.policies)
        };
    let id = explanation.chosen.ok_or_else(|| nowhere(&explanation))?;
    let given = given_on(&req, &seen.nodes, id)?;
    Ok((explanation, given))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Each volume `w` holds that a node must reach where it is: those in a shared or block
    /// class's store.
    async fn stored_volumes(&self, w: &Workload) -> Result<Vec<(Uuid, Home)>, AppError> {
        let page = Page::new(None, Page::MAX);
        let held = self.store.volumes(w.project, Some(w.id), false, page).await?;
        Ok(held
            .into_iter()
            .filter(|v| v.attached_to == Some(w.id) && v.deleted_at.is_none())
            .filter(|v| v.store.is_some())
            .map(|v| (v.id, v.home()))
            .collect())
    }

    /// What the scheduler reads of `nodes` for `w`, approved to be `target`.
    async fn inputs(
        &self,
        tx: &mut S::Tx,
        (w, target): (&Workload, &Intent),
        nodes: Vec<Node>,
    ) -> Result<Inputs, AppError> {
        let owners = self.owners(w.organization).await?;
        let ids: Vec<Uuid> = nodes.iter().map(|n| n.id).collect();
        let (entries, placed) = self.placed_entries(tx, w).await?;
        let promised = self.placed_promises(tx, w, (&entries, placed.as_ref())).await?;
        let addressing = self.addressing(tx, (w.id, &target.body), &owners, &nodes).await?;
        Ok(Inputs {
            allocations: tx.allocations(&ids).await?,
            held: tx.device_holds(&ids, w.id).await?,
            held_cores: tx.core_holds(&ids, w.id).await?,
            peers: tx.peers(w.organization, w.id).await?,
            volumes: self.stored_volumes(w).await?,
            needed: self.needed(w, &target.body).await?,
            mesh: self.judged().await?.pairs(),
            refused: tx.refusals(w.id, target.id).await?,
            learned: self.learned(tx, &ids, target).await?,
            policies: self.policies(&owners).await?,
            owners,
            nodes,
            entries,
            placed,
            promised,
            addressing,
        })
    }

    /// Where `w`, approved to be `target`, moves to: `named`, or the scheduler's choice among
    /// every node serving it but its own.
    pub(in crate::workload) async fn destination(
        &self,
        tx: &mut S::Tx,
        (w, target): (&Workload, &Intent),
        named: Option<Uuid>,
    ) -> Result<Chosen, AppError> {
        let unknown = || AppError::NotFound { kind: "node" };
        let placed = tx.placement_in(w.id).await?;
        let placed = placed.ok_or_else(|| AppError::conflict("workload.unplaced", "on no node"))?;
        let source = self.store.node(placed.node).await?.ok_or_else(unknown)?;
        if named == Some(source.id) {
            return Err(AppError::invalid("node", "the workload is on that node already"));
        }
        let nodes: Vec<Node> = if let Some(id) = named {
            vec![self.store.node(id).await?.ok_or_else(unknown)?]
        } else {
            let owners = self.owners(w.organization).await?;
            let serving = tx.serving_nodes(&owners).await?;
            serving.into_iter().filter(|n| n.id != source.id).collect()
        };
        let inputs = self.inputs(tx, (w, target), nodes).await?;
        let (explanation, given) = pick(&inputs, (w, target), named)?;
        let edge = inputs.addressing.stable();
        let id = explanation.chosen.ok_or_else(unknown)?;
        let node = inputs.nodes.into_iter().find(|n| n.id == id).ok_or_else(unknown)?;
        Ok(Chosen { source, node, explanation, given, edge })
    }
}
