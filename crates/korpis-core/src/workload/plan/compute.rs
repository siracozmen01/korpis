// SPDX-License-Identifier: AGPL-3.0-or-later

//! Computing a plan's steps: the difference between what the workload was approved to be and
//! what is now declared, as actions a node and the control plane can execute.

use super::step::{Classifier, Step, StepAction, StepExecutor};
use crate::object::{Kind, ObjectRef};
use crate::value::{Value, ValueMap};
use crate::workload::spec::{IntentBody, VolumeSpec};
use crate::workload::{Power, PowerState};
use uuid::Uuid;

struct Builder<'a> {
    classifier: Classifier<'a>,
    target: ObjectRef,
    steps: Vec<Step>,
}

fn int(n: u64) -> Value {
    Value::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

fn text(s: &str) -> Value {
    Value::String(s.to_owned())
}

impl Builder<'_> {
    fn push(&mut self, action: StepAction, detail: ValueMap, touched: &[String]) {
        let (disruptive, classified_by, reason) = self.classifier.classify(action, touched);
        let (_, reversible) = action.floor();
        self.steps.push(Step {
            index: u32::try_from(self.steps.len()).unwrap_or(u32::MAX),
            action,
            executor: StepExecutor::Node,
            target: self.target,
            reversible,
            disruptive,
            classified_by,
            reason,
            detail,
            timeout: None,
        });
    }

    fn pull(&mut self, body: &IntentBody) {
        let detail = ValueMap::from([("digest".to_owned(), text(&body.recipe.digest))]);
        self.push(StepAction::PullRecipe, detail, &[]);
    }

    fn add_volume(&mut self, v: &VolumeSpec) {
        let mut detail = ValueMap::from([("volume".to_owned(), text(&v.name))]);
        if let Some(id) = v.existing {
            detail.insert("existing".into(), text(&id.to_string()));
            self.push(StepAction::AttachVolume, detail, &[]);
        } else {
            detail.insert("size_bytes".into(), int(v.size_bytes));
            detail.insert("storage_class".into(), text(&v.storage_class));
            self.push(StepAction::CreateVolume, detail, &[]);
        }
    }
}

/// Configuration keys whose values differ.
fn changed_keys(a: &ValueMap, b: &ValueMap) -> Vec<String> {
    a.keys()
        .chain(b.keys())
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether moving from `a` to `b` needs the running instance restarted: anything the process is
/// started with changed.
fn needs_restart(a: &IntentBody, b: &IntentBody) -> bool {
    a.recipe.digest != b.recipe.digest
        || a.runtime != b.runtime
        || a.resources != b.resources
        || a.volumes != b.volumes
        || a.config != b.config
        || a.lifecycle != b.lifecycle
        || a.console != b.console
}

fn first_intent(b: &mut Builder<'_>, to: &IntentBody, power: PowerState) {
    b.pull(to);
    for v in &to.volumes {
        b.add_volume(v);
    }
    b.push(StepAction::Create, ValueMap::new(), &[]);
    if power == PowerState::Running {
        b.push(StepAction::Start, ValueMap::new(), &[]);
    }
}

fn changed_intent(b: &mut Builder<'_>, from: &IntentBody, to: &IntentBody, power: PowerState) {
    if from.recipe.digest != to.recipe.digest {
        b.pull(to);
    }
    for v in to.volumes.iter().filter(|v| !from.volumes.iter().any(|o| o.name == v.name)) {
        b.add_volume(v);
    }
    for v in from.volumes.iter().filter(|v| !to.volumes.iter().any(|n| n.name == v.name)) {
        let detail = ValueMap::from([("volume".to_owned(), text(&v.name))]);
        b.push(StepAction::DetachVolume, detail, &[]);
    }
    let touched = changed_keys(&from.config, &to.config);
    let restart = needs_restart(from, to);
    if restart
        || from.health != to.health
        || from.placement != to.placement
        || from.continuity != to.continuity
    {
        let fields = Value::List(touched.iter().map(|k| text(k)).collect());
        b.push(
            StepAction::Reconfigure,
            ValueMap::from([("config_keys".to_owned(), fields)]),
            &touched,
        );
    }
    if from.endpoints != to.endpoints {
        b.push(StepAction::ReconfigureNetwork, ValueMap::new(), &[]);
    }
    if restart && power == PowerState::Running {
        b.push(StepAction::Restart, ValueMap::new(), &[]);
    }
}

/// The steps of an intent plan for `workload`: from the approved body (`None` for a new
/// workload) to the declared one, given whether the workload is to be running. Identical bodies
/// give no steps.
#[must_use]
pub fn intent_steps(
    workload: Uuid,
    from: Option<&IntentBody>,
    to: &IntentBody,
    power: PowerState,
    classifier: Classifier<'_>,
) -> Vec<Step> {
    let mut b =
        Builder { classifier, target: ObjectRef::new(Kind::Workload, workload), steps: Vec::new() };
    match from {
        None => first_intent(&mut b, to, power),
        Some(from) => changed_intent(&mut b, from, to, power),
    }
    b.steps
}

/// The steps of a power plan for `workload`: a start, a stop, a restart, or nothing.
#[must_use]
pub fn power_steps(
    workload: Uuid,
    from: Power,
    to: Power,
    classifier: Classifier<'_>,
) -> Vec<Step> {
    let mut b =
        Builder { classifier, target: ObjectRef::new(Kind::Workload, workload), steps: Vec::new() };
    let detail = ValueMap::from([("cycle".to_owned(), int(to.cycle))]);
    match (from.desired, to.desired) {
        (PowerState::Stopped, PowerState::Running) => b.push(StepAction::Start, detail, &[]),
        (PowerState::Running, PowerState::Stopped) => b.push(StepAction::Stop, detail, &[]),
        (PowerState::Running, PowerState::Running) if to.cycle > from.cycle => {
            b.push(StepAction::Restart, detail, &[]);
        }
        _ => {}
    }
    b.steps
}
