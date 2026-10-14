// SPDX-License-Identifier: AGPL-3.0-or-later

//! The step of a prune plan (§5.3 of docs/06-storage.md): one, executed by the control plane,
//! naming the backups it forgets and why each other one is kept, so that what is approved is the
//! list of what goes. Executing it forgets no backup the list does not name.

use super::step::{Classifier, Step, StepAction, StepExecutor};
use crate::backup::retention::Judged;
use crate::object::{Kind, ObjectRef};
use crate::value::{Value, ValueMap};
use uuid::Uuid;

/// The steps of a plan pruning `workload`'s backups in `repository`, as `judged`.
#[must_use]
pub fn prune_steps(
    workload: Uuid,
    repository: Uuid,
    judged: &[Judged],
    classifier: Classifier<'_>,
) -> Vec<Step> {
    let action = StepAction::PruneBackups;
    let forget = judged.iter().filter(|j| j.kept.is_empty());
    let kept = judged.iter().filter(|j| !j.kept.is_empty()).map(|j| {
        let why = j.kept.iter().map(|k| Value::String(k.as_str().to_owned())).collect();
        (j.id.to_string(), Value::List(why))
    });
    let detail = ValueMap::from([
        ("repository".to_owned(), Value::String(repository.to_string())),
        (
            "forget".to_owned(),
            Value::List(forget.map(|j| Value::String(j.id.to_string())).collect()),
        ),
        ("kept".to_owned(), Value::Map(kept.collect())),
    ]);
    let (disruptive, classified_by, reason) = classifier.classify(action, &[]);
    vec![Step {
        index: 0,
        action,
        executor: StepExecutor::ControlPlane,
        target: ObjectRef::new(Kind::Workload, workload),
        reversible: action.floor().1,
        disruptive,
        classified_by,
        reason,
        detail,
        timeout: None,
    }]
}

/// The backups step `s` forgets, as its detail names them.
#[must_use]
pub fn forgotten(s: &Step) -> Vec<Uuid> {
    match s.detail.get("forget") {
        Some(Value::List(ids)) => ids
            .iter()
            .filter_map(|v| match v {
                Value::String(id) => Uuid::parse_str(id).ok(),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}
