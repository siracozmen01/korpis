// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declaring a workload that has runs (§4 of docs/05-scheduling.md): its schedule is read in a
//! zone the control plane's database knows, its policy is stored as it applies with the defaults
//! written out, and a policy on a workload that has no runs is refused.

use crate::recipes::{body, create, project, push_paper};
use crate::refusal::detail;
use crate::support::Harness;
use korpis_proto::korpis::v1 as pb;

fn scheduled(repo: &str, zone: &str) -> pb::IntentBody {
    pb::IntentBody {
        lifecycle: pb::Lifecycle::LIFECYCLE_SCHEDULED.into(),
        schedule: "0 3 * * *".into(),
        timezone: zone.to_owned(),
        ..body(repo, &[])
    }
}

#[tokio::test]
async fn a_scheduled_workload_is_read_in_a_known_zone_and_stores_its_policy_as_it_applies() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "jobs").await;
    let workloads = h.workloads(h.root());

    let declared = create("backup", &project, "backup", scheduled(&repo, "Europe/Berlin"));
    let declared = workloads.declare_intent(declared).await.unwrap().into_owned();
    let stored = declared.intent.into_option().unwrap().body.into_option().unwrap();
    assert_eq!(stored.timezone, "Europe/Berlin");
    let runs = stored.runs.into_option().expect("the policy, written out");
    assert_eq!(runs.concurrency.as_known(), Some(pb::Concurrency::CONCURRENCY_FORBID));
    assert_eq!((runs.keep_successful, runs.keep_failed), (Some(3), Some(10)));

    let atlantis = create("atlantis", &project, "atlantis", scheduled(&repo, "Nowhere/Atlantis"));
    let refused = workloads.declare_intent(atlantis).await.unwrap_err();
    assert_eq!(detail(&refused).field, "body.timezone", "{refused:?}");

    let persistent = pb::IntentBody { runs: pb::RunPolicy::default().into(), ..body(&repo, &[]) };
    let refused = workloads.declare_intent(create("p", &project, "p", persistent)).await;
    assert_eq!(detail(&refused.unwrap_err()).field, "body.runs");
}
