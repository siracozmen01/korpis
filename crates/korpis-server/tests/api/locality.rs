// SPDX-License-Identifier: AGPL-3.0-or-later

//! Content locality (§2.2 of docs/05-scheduling.md): a node that reported holding the recipe's
//! content is preferred over one alike in every other way, a node that never reported is not
//! scored for it, and a report naming anything but canonical digests is refused whole.

use crate::agent::{Stream, TestAgent, welcome};
use crate::recipes::{body, create, project, push_paper_signed_by};
use crate::support::{Harness, test_key};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

/// Node `name`, enrolled with its session open; its wire id and the session.
async fn node(h: &Harness, name: &str, seed: u8) -> (String, Stream) {
    let a = TestAgent::enroll(h, name, seed).await;
    let (s, first) = TestAgent::open(h, a.hello_at(SystemTime::now(), &[seed; 16])).await;
    welcome(&first.unwrap());
    (a.node.clone(), s)
}

async fn report(s: &mut Stream, present: &[&str]) {
    let status = agent::ContentStatus {
        present: present.iter().map(|d| (*d).to_owned()).collect(),
        ..Default::default()
    };
    let up = Up::ContentStatus(Box::new(status));
    s.send(agent::AgentMessage { message: Some(up), ..Default::default() }).await.unwrap();
}

/// How many nodes have reported, and how many digests they hold, once `reported` have.
async fn held(h: &Harness, reported: i64) -> i64 {
    let store = h.db.store().await.unwrap();
    for _ in 0..50 {
        let count =
            sqlx::query_scalar("SELECT count(*) FROM node WHERE content_reported_at IS NOT NULL");
        let n: i64 = count.fetch_one(store.pool()).await.unwrap();
        if n == reported {
            let rows = sqlx::query_scalar("SELECT count(*) FROM node_content");
            return rows.fetch_one(store.pool()).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("never {reported} nodes reported");
}

#[tokio::test]
async fn a_node_holding_the_recipe_is_preferred_over_one_alike_in_every_other_way() {
    let h = Harness::new().await;
    // Scored alike otherwise, the first enrolled would be preferred: its identifier is the lower.
    let (first, mut s1) = node(&h, "fra-1", 1).await;
    let (holder, mut s2) = node(&h, "fra-2", 2).await;
    let (repo, digest) = push_paper_signed_by(&test_key()).await;
    report(&mut s1, &[]).await;
    report(&mut s2, &[&digest]).await;
    assert_eq!(held(&h, 2).await, 1);
    report(&mut s1, &["sha256:NOT-CANONICAL"]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(held(&h, 2).await, 1, "refused whole");

    let project = project(&h, &h.root_org(), "games").await;
    let declared = create("d1", &project, "mc", body(&repo, &[]));
    let declared = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let get = pb::GetWorkloadRequest {
        id: declared.workload.as_option().unwrap().id.clone(),
        ..Default::default()
    };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    let placement = got.status.into_option().unwrap().placement.into_option().unwrap();
    assert_eq!(placement.node, holder);
    let why = placement.explanation.as_option().unwrap();
    let locality = |n: &str| {
        why.scored
            .iter()
            .find(|s| s.node == n && s.signal == "content_locality")
            .map(|s| (s.value, s.weight))
    };
    assert_eq!((locality(&holder), locality(&first)), (Some((1.0, 0.25)), Some((0.0, 0.25))));
}
