// SPDX-License-Identifier: AGPL-3.0-or-later

//! The step of a migration's plan (§8.1 of docs/05-scheduling.md): one `migrate`, which the
//! control plane executes by advancing the migration's phases on both nodes. Its detail is what
//! the approver reads: the two nodes, the hold, each volume with what crosses for it, whether the
//! workload is stopped or its state captured, and each address that changes.

use super::{Builder, int, text};
use crate::object::{Kind, ObjectRef};
use crate::value::{Value, ValueMap};
use crate::workload::migration::Crossing;
use crate::workload::plan::step::{Classifier, Step, StepAction, StepExecutor};
use std::time::Duration;
use uuid::Uuid;

/// A volume as a migration's plan names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crossed {
    /// The name the workload's declaration gives it.
    pub name: String,
    /// Its size.
    pub size_bytes: u64,
    /// How it crosses.
    pub crossing: Crossing,
}

/// What a migration's plan moves, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moving {
    /// The node it leaves.
    pub source: Uuid,
    /// The node it goes to.
    pub destination: Uuid,
    /// Whether an operator named it, rather than the scheduler choosing it.
    pub named: bool,
    /// How long the source keeps its copies after cutover.
    pub hold: Duration,
    /// Each volume it holds.
    pub volumes: Vec<Crossed>,
    /// Whether `quiesce` captures its state rather than stopping it.
    pub live: bool,
    /// The endpoints whose address changes: each `node_port` one.
    pub changed_addresses: Vec<String>,
}

/// Where a migration's plan moves its workload, as its step names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Destination {
    /// The node.
    pub node: Uuid,
    /// Whether an operator named it, so it is checked again as a placement by hand is; one the
    /// scheduler chose is checked again against every filter of the scheduler.
    pub named: bool,
    /// How long the source keeps its copies after cutover.
    pub hold: Duration,
}

/// Where a migration's plan moves its workload, as its step names it.
#[must_use]
pub fn destination_of(steps: &[Step]) -> Option<Destination> {
    let step = steps.iter().find(|s| s.action == StepAction::Migrate)?;
    let node = match step.detail.get("destination") {
        Some(Value::String(s)) => Uuid::parse_str(s).ok()?,
        _ => return None,
    };
    let hold = match step.detail.get("hold_seconds") {
        Some(Value::Int(n)) => Duration::from_secs(u64::try_from(*n).ok()?),
        _ => return None,
    };
    // A step that does not say who chose its node is checked as the scheduler's choice, the
    // stricter of the two.
    let named = matches!(step.detail.get("chosen_by"), Some(Value::String(s)) if s == "operator");
    Some(Destination { node, named, hold })
}

/// The steps of a migration's plan for `workload`: one `migrate`, executed by the control plane,
/// disruptive at the vocabulary's floor since `quiesce` stops the workload.
#[must_use]
pub fn migrate_steps(workload: Uuid, m: &Moving, classifier: Classifier<'_>) -> Vec<Step> {
    let mut b =
        Builder { classifier, target: ObjectRef::new(Kind::Workload, workload), steps: Vec::new() };
    let volumes = m
        .volumes
        .iter()
        .map(|v| {
            Value::Map(ValueMap::from([
                ("volume".to_owned(), text(&v.name)),
                ("size_bytes".to_owned(), int(v.size_bytes)),
                ("crossing".to_owned(), text(v.crossing.as_str())),
            ]))
        })
        .collect();
    let changed = m.changed_addresses.iter().map(|e| text(e)).collect();
    let detail = ValueMap::from([
        ("source".to_owned(), text(&m.source.to_string())),
        ("destination".to_owned(), text(&m.destination.to_string())),
        ("chosen_by".to_owned(), text(if m.named { "operator" } else { "scheduler" })),
        ("hold_seconds".to_owned(), int(m.hold.as_secs())),
        ("volumes".to_owned(), Value::List(volumes)),
        ("quiesce".to_owned(), text(if m.live { "capture" } else { "stop" })),
        ("addresses_changing".to_owned(), Value::List(changed)),
    ]);
    b.push(StepAction::Migrate, detail, &[]);
    for s in &mut b.steps {
        s.executor = StepExecutor::ControlPlane;
    }
    b.steps
}
