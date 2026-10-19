// SPDX-License-Identifier: AGPL-3.0-or-later

//! Computing plans.

use crate::support::{GIB, body, classifier, game_server};
use StepAction as A;
use korpis_core::recipe::Disruption;
use korpis_core::value::Value;
use korpis_core::workload::plan::compute::{intent_steps, power_steps};
use korpis_core::workload::plan::impact::{assess, delta};
use korpis_core::workload::plan::{Classifier, DriverClass, Estimate, Layer, Override, StepAction};
use korpis_core::workload::{Power, PowerChange, PowerState};
use uuid::Uuid;

fn actions(steps: &[korpis_core::workload::plan::Step]) -> Vec<StepAction> {
    steps.iter().map(|s| s.action).collect()
}

#[test]
fn a_new_workload_pulls_creates_and_starts_without_disruption() {
    let (driver, recipe) = (DriverClass::default(), game_server());
    let steps = intent_steps(
        Uuid::now_v7(),
        (None, &body()),
        None,
        PowerState::Running,
        classifier(&driver, &recipe),
    )
    .unwrap();
    assert_eq!(actions(&steps), [A::PullRecipe, A::CreateVolume, A::Create, A::Start]);
    assert!(steps.iter().all(|s| !s.disruptive && s.reversible));
    assert_eq!(steps.iter().map(|s| s.index).collect::<Vec<_>>(), [0, 1, 2, 3]);
    let impact = assess(&steps, delta(None, Some(&body())));
    assert_eq!(impact.downtime, Estimate::Known(std::time::Duration::ZERO));
    assert_eq!(
        (impact.cost_delta.memory_bytes, impact.cost_delta.workloads),
        (i64::try_from(4 * GIB).unwrap(), 1)
    );
}

#[test]
fn a_changed_intent_is_the_difference_and_only_the_difference() {
    let (driver, recipe) = (DriverClass::default(), game_server());
    let c = classifier(&driver, &recipe);
    let id = Uuid::now_v7();
    assert!(
        intent_steps(id, (Some(&body()), &body()), None, PowerState::Running, c)
            .unwrap()
            .is_empty()
    );

    let mut motd = body();
    motd.config.insert("motd".into(), Value::String("Season 5".into()));
    let steps = intent_steps(id, (Some(&body()), &motd), None, PowerState::Running, c).unwrap();
    assert_eq!(actions(&steps), [StepAction::Reconfigure, StepAction::Restart]);
    assert!(!steps[0].disruptive, "motd is not a disruptive field");
    assert_eq!((steps[1].disruptive, steps[1].classified_by), (true, Layer::Recipe));
    let stopped = intent_steps(id, (Some(&body()), &motd), None, PowerState::Stopped, c).unwrap();
    assert_eq!(actions(&stopped), [StepAction::Reconfigure], "nothing to restart");

    let mut hard = body();
    hard.config.insert("difficulty".into(), Value::String("hard".into()));
    let steps = intent_steps(id, (Some(&body()), &hard), None, PowerState::Stopped, c).unwrap();
    assert_eq!((steps[0].disruptive, steps[0].classified_by), (true, Layer::Recipe));

    let mut smaller = body();
    smaller.volumes.clear();
    smaller.endpoints.clear();
    let steps = intent_steps(id, (Some(&body()), &smaller), None, PowerState::Running, c).unwrap();
    assert_eq!(
        actions(&steps),
        [A::DetachVolume, A::Reconfigure, A::ReconfigureNetwork, A::Restart]
    );
    assert!(steps[0].reversible, "detaching retains the data");
    let d = delta(Some(&body()), Some(&smaller));
    assert_eq!((d.volumes, d.endpoints), (-1, -1), "a plan can shrink a workload");
}

#[test]
fn power_plans_are_starts_stops_and_restarts() {
    let (driver, recipe) = (DriverClass::default(), Disruption::default());
    let c = classifier(&driver, &recipe);
    let id = Uuid::now_v7();
    let on = Power::initial(PowerState::Running);
    let off = on.next(PowerChange::Stop);
    assert_eq!((off.version, off.desired), (2, PowerState::Stopped));
    assert_eq!(actions(&power_steps(id, on, off, c)), [StepAction::Stop]);
    assert_eq!(
        actions(&power_steps(id, off, off.next(PowerChange::Start), c)),
        [StepAction::Start]
    );
    let again = on.next(PowerChange::Restart);
    assert_eq!((again.cycle, again.version), (1, 2));
    assert_eq!(actions(&power_steps(id, on, again, c)), [StepAction::Restart]);
    assert_eq!(
        actions(&power_steps(id, off, off.next(PowerChange::Restart), c)),
        [StepAction::Start]
    );
    assert!(power_steps(id, on, Power { version: 2, ..on }, c).is_empty());
}

#[test]
fn a_stop_says_the_longest_it_can_take_the_recipe_s_hook_then_the_grace() {
    let (driver, recipe) = (DriverClass::default(), Disruption::default());
    let within = |c, to| {
        let on = Power::initial(PowerState::Running);
        let steps = power_steps(Uuid::now_v7(), on, on.next(to), c);
        steps[0].detail.get("stops_within_seconds").cloned()
    };
    let bare = classifier(&driver, &recipe);
    assert_eq!(within(bare, PowerChange::Stop), Some(Value::Int(30)), "the grace alone");
    let hooked = Classifier { stop_wait: std::time::Duration::from_secs(120), ..bare };
    assert_eq!(within(hooked, PowerChange::Stop), Some(Value::Int(150)), "its hook's wait too");
    assert_eq!(within(hooked, PowerChange::Restart), Some(Value::Int(150)), "a restart stops");
    let off = Power::initial(PowerState::Running).next(PowerChange::Stop);
    let start = power_steps(Uuid::now_v7(), off, off.next(PowerChange::Start), hooked);
    assert!(!start[0].detail.contains_key("stops_within_seconds"), "a start stops nothing");
}

#[test]
fn the_override_chain_only_raises_below_the_operator_and_never_below_the_floor() {
    let id = Uuid::now_v7();
    let (on, off) = (
        Power::initial(PowerState::Running),
        Power::initial(PowerState::Running).next(PowerChange::Stop),
    );
    let quiet = Disruption::default();
    let vm = DriverClass { disruptive: vec![StepAction::Stop] };
    let stop = |driver: &DriverClass, recipe: &Disruption, overrides: &[Override]| {
        let c = korpis_core::workload::plan::Classifier {
            driver,
            recipe,
            overrides,
            stop_wait: std::time::Duration::ZERO,
        };
        let s = power_steps(id, on, off, c).remove(0);
        (s.disruptive, s.classified_by)
    };
    assert_eq!(stop(&DriverClass::default(), &quiet, &[]), (false, Layer::Vocabulary));
    assert_eq!(stop(&vm, &quiet, &[]), (true, Layer::Driver));
    assert_eq!(stop(&DriverClass::default(), &game_server(), &[]), (true, Layer::Recipe));
    let relax = [Override { action: StepAction::Stop, disruptive: false }];
    assert_eq!(stop(&vm, &game_server(), &relax), (false, Layer::Operator), "on the record");
    let c = korpis_core::workload::plan::Classifier {
        driver: &vm,
        recipe: &quiet,
        overrides: &[Override { action: StepAction::DeleteVolume, disruptive: false }],
        stop_wait: std::time::Duration::ZERO,
    };
    assert!(c.classify(StepAction::DeleteVolume, &[]).0, "the floor cannot be relaxed");
}
