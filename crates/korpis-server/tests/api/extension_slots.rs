// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the panel shows in a slot: the contributions of the installations covering an object,
//! each only to a viewer who may invoke its action there.

use crate::extensions::push_dns;
use crate::providers::installed_from;
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use korpis_proto::korpis::v1 as pb;

async fn slots(
    h: &Harness,
    as_: connectrpc::client::ClientConfig,
    object: &str,
) -> Vec<pb::SlotContribution> {
    let req = pb::ListSlotsRequest {
        slot: "workload.panel".into(),
        object: object.to_owned(),
        ..Default::default()
    };
    h.extensions(as_).list_slots(req).await.unwrap().into_owned().contributions
}

#[tokio::test]
async fn a_workloads_page_shows_what_its_viewer_may_invoke_there() {
    let h = Harness::new().await;
    let x = installed_from(&h, 41, "dns", push_dns().await).await;
    let games = project(&h, &h.root_org(), "games").await;
    let declared = create("w", &games, "w", body(&push_paper().await, &[]));
    let out = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let workload = out.workload.as_option().unwrap().id.clone();

    let shown = slots(&h, h.root(), &workload).await;
    assert_eq!(shown.len(), 1);
    let form = &shown[0];
    assert_eq!((form.extension.as_str(), form.kind.as_str()), (x.extension.id.as_str(), "form"));
    assert_eq!(form.action.as_option().unwrap().name, "purge");

    // A viewer who may read the workload but not invoke the action sees nothing offered.
    let (token, secret) = h.bare_token().await;
    h.issue_to(token, "reader", &["workload.read"]).await;
    let reader = h.config(Some(secret.expose()));
    assert!(slots(&h, reader, &workload).await.is_empty());
}
