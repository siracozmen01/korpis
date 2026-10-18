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
use korpis_core::workload::assignment::Assignment;
use korpis_core::workload::dependency::ResolvedDependency;
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
                ..Default::default()
            })
            .collect(),
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
        shaping: agent::Shaping::default().into(),
        plan: a
            .plan
            .as_ref()
            .map(|p| agent::NodePlan {
                plan: wire_id(IdKind::Plan, p.plan),
                steps: p.steps.iter().map(step_to_pb).collect(),
                through: p.through,
                whole: plan_to_pb(&p.whole).into(),
                ..Default::default()
            })
            .into(),
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
        runs: a.runs.iter().map(crate::run::ref_to_pb).collect(),
        ..Default::default()
    }
}
