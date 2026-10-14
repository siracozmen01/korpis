// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a plan needs before it applies.

use crate::support::{body, classifier, game_server};
use korpis_core::object::{Kind, ObjectRef};
use korpis_core::recipe::Disruption;
use korpis_core::scope::Scope;
use korpis_core::workload::plan::approval::required;
use korpis_core::workload::plan::compute::{intent_steps, power_steps};
use korpis_core::workload::plan::impact::{ResourceDelta, assess};
use korpis_core::workload::plan::{Approval, DriverClass, Proposing, Step, StepAction};
use korpis_core::workload::{Power, PowerChange, PowerState};
use uuid::Uuid;

fn needs(steps: &[Step], proposing: &Proposing) -> Approval {
    required(steps, &assess(steps, ResourceDelta::default()), proposing).0
}

#[test]
fn routine_plans_apply_at_once_and_disruptive_ones_wait_for_their_proposer() {
    let driver = DriverClass::default();
    let id = Uuid::now_v7();
    let on = Power::initial(PowerState::Running);
    let start = intent_steps(
        id,
        (None, &body()),
        None,
        PowerState::Running,
        classifier(&driver, &game_server()),
    )
    .unwrap();
    assert_eq!(needs(&start, &Proposing::Permitted), Approval::Auto, "pressing Start is one click");

    let quiet = Disruption::default();
    let stop = power_steps(id, on, on.next(PowerChange::Stop), classifier(&driver, &quiet));
    assert_eq!(
        needs(&stop, &Proposing::Permitted),
        Approval::Auto,
        "a stateless service stops freely"
    );

    let recipe = game_server();
    let stop = power_steps(id, on, on.next(PowerChange::Stop), classifier(&driver, &recipe));
    let (approval, why) =
        required(&stop, &assess(&stop, ResourceDelta::default()), &Proposing::Permitted);
    assert_eq!(approval, Approval::Confirmation, "a game server's operator confirms the stop");
    assert!(why.contains("players"), "{why}");
}

#[test]
fn irreversible_steps_always_wait_and_propose_only_grants_need_someone_else() {
    let mut steps = power_steps(
        Uuid::now_v7(),
        Power::initial(PowerState::Stopped),
        Power::initial(PowerState::Stopped).next(PowerChange::Start),
        classifier(&DriverClass::default(), &Disruption::default()),
    );
    steps[0].action = StepAction::DeleteVolume;
    steps[0].reversible = false;
    assert_eq!(needs(&steps, &Proposing::Permitted), Approval::Confirmation);

    let scope = Scope::Object(ObjectRef::new(Kind::Project, Uuid::now_v7()));
    let proposing = Proposing::ProposeOnly { approval_scope: scope.clone() };
    assert_eq!(needs(&[], &proposing), Approval::Authority { scope }, "even an empty plan");
}
