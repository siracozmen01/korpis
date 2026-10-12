// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading workloads (§4.3 of docs/02-architecture.md): states the control plane can state
//! without a node, and nothing it cannot.

use crate::lifecycle::{approve, workload};
use crate::recipes::{body, change, text};
use crate::support::{Harness, reason};
use korpis_proto::korpis::v1 as pb;

async fn status(h: &Harness, w: &str) -> pb::WorkloadStatus {
    h.workloads(h.root())
        .get_workload(pb::GetWorkloadRequest { id: w.to_owned(), ..Default::default() })
        .await
        .unwrap()
        .into_owned()
        .status
        .into_option()
        .unwrap()
}

fn convergence(s: &pb::WorkloadStatus) -> Option<pb::Convergence> {
    s.convergence.as_known()
}

#[tokio::test]
async fn a_workload_reads_as_what_was_declared_approved_and_no_more() {
    let h = Harness::new().await;
    let (w, repo) = workload(&h).await;
    let s = status(&h, &w).await;
    assert_eq!(convergence(&s), Some(pb::Convergence::CONVERGENCE_UNPLACED), "no node yet");
    assert_eq!(s.head.as_option().unwrap().id, s.target.as_option().unwrap().id);
    assert!(!s.observation.is_set(), "nothing is reported that no node said (P4)");
    let hard = body(&repo, &[("difficulty", text("hard"))]);
    let declared =
        h.workloads(h.root()).declare_intent(change("d", &w, 1, hard)).await.unwrap().into_owned();
    let s = status(&h, &w).await;
    assert_eq!(convergence(&s), Some(pb::Convergence::CONVERGENCE_AWAITING_APPROVAL));
    assert_ne!(s.head.as_option().unwrap().id, s.target.as_option().unwrap().id);
    let plan = declared.plan.as_option().unwrap().id.clone();
    assert_eq!(s.active_plan.as_option().unwrap().id, plan);
    let mut later = approve("a", &plan);
    later.apply_after = korpis_server::api::wire::timestamp(
        std::time::SystemTime::now() + std::time::Duration::from_secs(3600),
    )
    .into();
    h.plans(h.root()).approve_plan(later).await.unwrap();
    assert_eq!(convergence(&status(&h, &w).await), Some(pb::Convergence::CONVERGENCE_SCHEDULED));
    let intents = h
        .workloads(h.root())
        .list_intents(pb::ListIntentsRequest { workload: w.clone(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(intents.intents.iter().map(|i| i.version).collect::<Vec<_>>(), [1, 2]);
}

#[tokio::test]
async fn listing_needs_a_place_and_a_reader_who_may_not_read_sees_nothing() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    let workloads = h.workloads(h.root());
    let e = workloads.list_workloads(pb::ListWorkloadsRequest::default()).await.unwrap_err();
    assert_eq!(crate::support::detail(&e).field, "project");
    let listed = workloads
        .list_workloads(pb::ListWorkloadsRequest {
            organization: Some(h.root_org()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(listed.workloads.len(), 1);
    let none = workloads
        .list_workloads(pb::ListWorkloadsRequest {
            organization: Some(h.root_org()),
            selector: pb::LabelSelector {
                match_labels: [("tier".to_owned(), "gold".to_owned())].into_iter().collect(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert!(none.workloads.is_empty());
    let (_, secret) = h.bare_token().await;
    let e = h
        .workloads(h.config(Some(secret.expose())))
        .get_workload(pb::GetWorkloadRequest { id: w, ..Default::default() })
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "object.not_found", "existence is gated by read authority");
}
