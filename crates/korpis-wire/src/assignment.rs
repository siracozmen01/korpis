// SPDX-License-Identifier: AGPL-3.0-or-later

//! Assignments: `korpis.agent.v1.Assignment` (§4.8 of docs/02-architecture.md). The bytes of the
//! encoded message are what the control plane signs, so the encoding is deterministic: every map
//! in the schema is ordered.

use crate::endpoint::endpoint_to_pb;
use crate::plan::{plan_to_pb, step_to_pb};
use crate::spec::continuity_to_pb;
use crate::wire_id;
use crate::workload::{intent_to_pb, power_to_pb};
use korpis_core::id::IdKind;
use korpis_core::tenancy::quota::Exhaustion;
use korpis_core::workload::assignment::Assignment;
use korpis_core::workload::dependency::{OverlayDependent, ResolvedDependency};
use korpis_proto::korpis::agent::v1 as agent;

/// The wire form of a dependency as its node is told it.
fn resolved_to_pb(d: &ResolvedDependency) -> agent::ResolvedDependency {
    agent::ResolvedDependency {
        name: d.name.clone(),
        inject: crate::spec::dependencies::injection_to_pb(d.inject).into(),
        required: d.required,
        targets: d
            .targets
            .iter()
            .map(|t| agent::DependencyEndpoint {
                workload: wire_id(IdKind::Workload, t.workload),
                endpoint: t.endpoint.clone(),
                container_port: u32::from(t.container_port),
                protocol: crate::spec::endpoints::protocol_to_pb(t.protocol).into(),
                address: t.address.map(|a| a.to_string()).unwrap_or_default(),
                ready: t.ready,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// The wire form of a dependent on another node of the mesh.
fn dependent_to_pb(d: &OverlayDependent) -> agent::OverlayDependent {
    agent::OverlayDependent {
        workload: wire_id(IdKind::Workload, d.workload),
        address: d.address.to_string(),
        target_address: d.target_address.to_string(),
        container_port: u32::from(d.container_port),
        protocol: crate::spec::endpoints::protocol_to_pb(d.protocol).into(),
        ..Default::default()
    }
}

fn peers_to_pb(r: &korpis_core::workload::peers::RuleAddresses) -> agent::RulePeers {
    agent::RulePeers {
        endpoint: r.place.endpoint.clone().unwrap_or_default(),
        rule: r.place.index,
        addresses: r.addresses.iter().map(ToString::to_string).collect(),
        ..Default::default()
    }
}

fn dedicated_to_pb(d: &korpis_core::address::Dedicated) -> agent::DedicatedAddress {
    agent::DedicatedAddress {
        address: d.address.to_string(),
        interface: d.interface.clone(),
        ..Default::default()
    }
}

/// What a spent egress budget holds the workload to, as its node is told it.
fn shaping_to_pb(held: Option<Exhaustion>) -> agent::Shaping {
    match held {
        None => agent::Shaping::default(),
        Some(Exhaustion::Floor(bits)) => agent::Shaping {
            egress_exhausted: true,
            egress_floor_bits_per_sec: bits,
            ..Default::default()
        },
        Some(Exhaustion::CutOff) => {
            agent::Shaping { egress_exhausted: true, egress_cut_off: true, ..Default::default() }
        }
    }
}

/// The wire form of the plan a node applies.
fn node_plan_to_pb(p: &korpis_core::workload::assignment::NodePlan) -> agent::NodePlan {
    agent::NodePlan {
        plan: wire_id(IdKind::Plan, p.plan),
        steps: p.steps.iter().map(step_to_pb).collect(),
        through: p.through,
        whole: plan_to_pb(&p.whole).into(),
        ..Default::default()
    }
}

/// The wire form of an assignment.
#[must_use]
pub fn assignment_to_pb(a: &Assignment) -> agent::Assignment {
    agent::Assignment {
        workload: wire_id(IdKind::Workload, a.workload),
        project: wire_id(IdKind::Project, a.project),
        organization: wire_id(IdKind::Organization, a.organization),
        workload_name: a.name.as_str().to_owned(),
        workload_labels: a.labels.as_map().clone().into_iter().collect(),
        generation: a.generation,
        target: intent_to_pb(&a.target).into(),
        power: power_to_pb(&a.power).into(),
        on_lease_expiry: continuity_to_pb(a.on_lease_expiry).into(),
        shaping: shaping_to_pb(a.egress).into(),
        plan: a.plan.as_ref().map(node_plan_to_pb).into(),
        retire: a
            .retire
            .map(|plan| agent::Retire { plan: wire_id(IdKind::Plan, plan), ..Default::default() })
            .into(),
        recipe_manifest: a.recipe_manifest.clone(),
        node: wire_id(IdKind::Node, a.node),
        lease_epoch: a.lease_epoch,
        uid_base: a.ids.base,
        uid_count: a.ids.count,
        residual: a.residual,
        volume_ids: a.volume_ids.clone().into_iter().collect(),
        endpoints: a.endpoints.iter().map(|e| endpoint_to_pb(e, &[])).collect(),
        reaches_api: a.reaches_api,
        dependencies: a.dependencies.iter().map(resolved_to_pb).collect(),
        challenges: a
            .challenges
            .iter()
            .map(|c| agent::AcmeChallenge {
                token: c.token.clone(),
                key_authorization: c.key_authorization.clone(),
                ..Default::default()
            })
            .collect(),
        alpn_challenges: a
            .alpn_challenges
            .iter()
            .map(|c| agent::AlpnChallenge {
                name: c.name.clone(),
                digest: c.digest.clone(),
                ..Default::default()
            })
            .collect(),
        runs: a.runs.iter().map(crate::run::ref_to_pb).collect(),
        kept_runs: a.kept_runs.iter().map(|r| wire_id(IdKind::Run, *r)).collect(),
        devices: crate::node::holds_to_pb(&a.devices),
        cores: a.cores.clone(),
        dependents: a.dependents.iter().map(dependent_to_pb).collect(),
        peers: a.peers.iter().map(peers_to_pb).collect(),
        dedicated: a.dedicated.as_ref().map(dedicated_to_pb).into(),
        ..Default::default()
    }
}
