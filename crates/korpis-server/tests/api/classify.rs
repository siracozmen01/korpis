// SPDX-License-Identifier: AGPL-3.0-or-later

//! The operator's layer of the classification chain (§3.3 of docs/01-model.md): set on the
//! record, under the workload's version, by `workload.classify`, and never below the floor.

use crate::lifecycle::{power, workload};
use crate::support::{Harness, detail, reason};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

fn set(
    key: &str,
    w: &str,
    version: Option<u64>,
    given: &[(&str, bool)],
    why: &str,
) -> pb::SetClassificationRequest {
    pb::SetClassificationRequest {
        mutation: pb::Mutation {
            idempotency_key: key.to_owned(),
            expected_version: version,
            ..Default::default()
        }
        .into(),
        workload: w.to_owned(),
        overrides: given
            .iter()
            .map(|(a, d)| pb::ClassificationOverride {
                action: (*a).to_owned(),
                disruptive: *d,
                ..Default::default()
            })
            .collect(),
        reason: why.to_owned(),
        ..Default::default()
    }
}

async fn version(h: &Harness, w: &str) -> u64 {
    let got = h
        .workloads(h.root())
        .get_workload(pb::GetWorkloadRequest { id: w.to_owned(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    got.status.as_option().unwrap().workload.as_option().unwrap().version
}

#[tokio::test]
async fn an_operator_makes_a_stop_routine_on_the_record_and_the_next_stop_needs_nobody() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    let workloads = h.workloads(h.root());
    let at = version(&h, &w).await;
    let why = "players are moved off before every stop";

    let e = workloads.set_classification(set("c0", &w, None, &[], why)).await.unwrap_err();
    assert_eq!(detail(&e).field, "mutation.expected_version");
    let e = workloads.set_classification(set("c1", &w, Some(at), &[], " ")).await.unwrap_err();
    assert_eq!(detail(&e).field, "reason", "the override is recorded with its reason");
    let floor = [("retire", false)];
    let e = workloads.set_classification(set("c2", &w, Some(at), &floor, why)).await.unwrap_err();
    assert_eq!(detail(&e).field, "overrides", "no override relaxes the vocabulary's floor");
    let twice = [("stop", false), ("stop", true)];
    let e = workloads.set_classification(set("c3", &w, Some(at), &twice, why)).await.unwrap_err();
    assert_eq!(detail(&e).field, "overrides");

    let routine = [("stop", false)];
    let answer =
        workloads.set_classification(set("c4", &w, Some(at), &routine, why)).await.unwrap();
    let classified = answer.into_owned().workload.into_option().unwrap();
    assert_eq!((classified.classification.len(), classified.version), (1, at + 1));
    let effects = h.effects("workload.classify").await;
    assert_eq!(
        effects.iter().map(|(o, g)| (o.as_str(), g.is_some())).collect::<Vec<_>>(),
        [("succeeded", true)]
    );
    let e = workloads.set_classification(set("c5", &w, Some(at), &[], why)).await.unwrap_err();
    assert_eq!(reason(&e), "workload.version_moved", "a stale version never overwrites");

    let stopped = workloads.stop(power("s1", &w, 1)).await.unwrap().into_owned();
    let plan = stopped.plan.as_option().unwrap();
    assert_eq!(plan.approval.as_known(), Some(pb::PlanApproval::PLAN_APPROVAL_AUTO));
    let stop = plan.steps.iter().find(|s| s.action == "stop").unwrap();
    assert!(!stop.disruptive, "the recipe said disruptive; the operator said otherwise");
    assert_eq!(stop.classified_by.as_known(), Some(pb::StepLayer::STEP_LAYER_OPERATOR));
    // The longest the stop can take is shown on the step: the example recipe's hook waits two
    // minutes for the server to exit, then the node's grace (§3 of docs/09-recipes.md).
    let within = stop.detail.get("stops_within_seconds").and_then(|v| match v.kind {
        Some(pb::__buffa::oneof::value::Kind::IntValue(n)) => Some(n),
        _ => None,
    });
    assert_eq!(within, Some(150), "{:?}", stop.detail);
}

#[tokio::test]
async fn classifying_is_its_own_authority_and_workload_write_does_not_imply_it() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    let (token, secret) = h.bare_token().await;
    h.issue_to(token, "g1", &["workload.read", "workload.write", "workload.stop"]).await;
    let writer = h.workloads(h.config(Some(secret.expose())));
    let at = version(&h, &w).await;
    let e = writer
        .set_classification(set("c1", &w, Some(at), &[("stop", false)], "quieter"))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
}
