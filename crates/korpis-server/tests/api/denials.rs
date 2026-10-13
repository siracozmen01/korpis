// SPDX-License-Identifier: AGPL-3.0-or-later

//! Refusals are recorded and coalesced (§10 of docs/10-api.md, §4 of
//! docs/15-observability.md): the first of a run at once, its repeats as one effect with a count
//! and the window's bounds once the window closes. The harness's window is one second.

use crate::lifecycle::power;
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

async fn denials(h: &Harness, workload: &str) -> Vec<pb::Effect> {
    let listed = h
        .effect_log(h.root())
        .list_effects(pb::ListEffectsRequest {
            workload: Some(workload.to_owned()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let denied =
        |e: &pb::Effect| e.outcome.as_known() == Some(pb::EffectOutcome::EFFECT_OUTCOME_DENIED);
    listed.effects.into_iter().filter(denied).collect()
}

#[tokio::test]
async fn a_retry_loop_is_one_effect_for_its_first_refusal_and_one_for_its_repeats() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    let (token, secret) = h.bare_token().await;
    h.issue_to(token, "g1", &["workload.read"]).await;
    let reader = h.workloads(h.config(Some(secret.expose())));

    for i in 0..5 {
        let e = reader.stop(power(&format!("s{i}"), &workload, 1)).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::PermissionDenied);
    }
    let first = denials(&h, &workload).await;
    assert_eq!(first.len(), 1, "the first refusal is in the log at once, its repeats are not");
    let refusal = first[0].clone();
    assert_eq!(refusal.count, 1);
    let missing = refusal.error.as_option().unwrap().detail.get("missing_action");
    assert!(missing.is_some(), "it names the action that was missing");

    let mut log = first;
    for _ in 0..40 {
        if log.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        log = denials(&h, &workload).await;
    }
    assert_eq!(log.len(), 2, "the window closed and its repeats were written");
    let repeats = &log[1];
    assert_eq!((repeats.action.as_str(), repeats.count), (refusal.action.as_str(), 4));
    let (from, to) = (repeats.occurred_at.as_option(), repeats.window_end.as_option());
    let (from, to) = (from.unwrap(), to.unwrap());
    assert!((from.seconds, from.nanos) <= (to.seconds, to.nanos), "with the window's bounds");

    reader.stop(power("s5", &workload, 1)).await.unwrap_err();
    let log = denials(&h, &workload).await;
    assert_eq!(log.len(), 3, "a refusal after the window opens a new run, written at once");
    assert_eq!(log[2].count, 1);
}
