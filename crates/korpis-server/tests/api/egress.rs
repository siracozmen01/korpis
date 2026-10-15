// SPDX-License-Identifier: AGPL-3.0-or-later

//! The egress default's one exception (§6 of docs/07-networking.md): the control plane marks the
//! assignment of an installed extension's workload, and no other, as reaching its API; the node
//! opens the API's address to it and nothing else of the control plane's.

use crate::providers::installed;
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use buffa::Message as _;
use korpis_proto::korpis::agent::v1 as agent;
use std::time::Duration;

/// Whether the latest assignment of the workload with wire id `workload` says it reaches the
/// API, once it has one.
async fn reaches_api(h: &Harness, workload: &str) -> bool {
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload).unwrap();
    let store = h.db.store().await.unwrap();
    for _ in 0..100 {
        let body: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT body FROM assignment WHERE workload_id = $1 ORDER BY generation DESC LIMIT 1",
        )
        .bind(id.uuid())
        .fetch_optional(store.pool())
        .await
        .unwrap();
        if let Some(b) = body {
            return agent::Assignment::decode_from_slice(&b).unwrap().reaches_api;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{workload} was never assigned");
}

#[tokio::test]
async fn an_installed_extension_s_workload_reaches_the_api_and_no_other_does() {
    let h = Harness::new().await;
    let x = installed(&h, 41, "dns").await;
    assert!(reaches_api(&h, &x.extension.workload).await, "the extension's workload");

    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declare = create("d1", &project, "mc", body(&repo, &[]));
    let out = h.workloads(h.root()).declare_intent(declare).await.unwrap().into_owned();
    let plain = out.workload.as_option().unwrap().id.clone();
    assert!(!reaches_api(&h, &plain).await, "a workload that is no extension");
}
