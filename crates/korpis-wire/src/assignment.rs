// SPDX-License-Identifier: AGPL-3.0-or-later

//! Assignments: `korpis.agent.v1.Assignment` (§4.8 of docs/02-architecture.md). The bytes of the
//! encoded message are what the control plane signs, so the encoding is deterministic: every map
//! in the schema is ordered.

use crate::plan::step_to_pb;
use crate::spec::continuity_to_pb;
use crate::wire_id;
use crate::workload::{intent_to_pb, power_to_pb};
use korpis_core::id::IdKind;
use korpis_core::workload::assignment::Assignment;
use korpis_proto::korpis::agent::v1 as agent;

/// The wire form of an assignment.
#[must_use]
pub fn assignment_to_pb(a: &Assignment) -> agent::Assignment {
    agent::Assignment {
        workload: wire_id(IdKind::Workload, a.workload),
        project: wire_id(IdKind::Project, a.project),
        organization: wire_id(IdKind::Organization, a.organization),
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
        ..Default::default()
    }
}
