// SPDX-License-Identifier: AGPL-3.0-or-later

//! A shared storage class (§2.1 of docs/06-storage.md): two nodes declaring a class of one
//! store. A volume a node reports in it is the store's, with no node, and the same volume
//! whichever node reports it; a workload holding one is told to stop when its lease cannot be
//! renewed, as its class is attachable from more than one node; and a workload attaching it is
//! placed on the other node, which reaches the store, once the first is cordoned. A node of
//! another organization declaring the same store is not given it (Finding 201).

use crate::agent::{TestAgent, welcome};
use crate::edge_groups::cordon;
use crate::journal::{ack, assignment, complete, send, until};
use crate::lifecycle::approve;
use crate::recipes::{body, create, project, push_paper};
use crate::streams::held_and_released;
use crate::support::Harness;
use crate::volume_delete::retired;
use crate::volumes::{ensured, list, volume_record};
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

const STORE: &str = "0b9c1f40-3d2e-4c55-9f0a-7e6d5c4b3a21";

/// The classes of a node mounting the store: its own disk's, and the shared one.
fn classes() -> Vec<pb::StorageClassInfo> {
    let local = pb::StorageClassInfo {
        name: "local".into(),
        backend: pb::StorageBackend::STORAGE_BACKEND_DIRECTORY.into(),
        capabilities: pb::StorageCapabilities {
            fencing: pb::Fencing::FENCING_NONE.into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    };
    let shared = pb::StorageClassInfo {
        name: "shared".into(),
        backend: pb::StorageBackend::STORAGE_BACKEND_NETWORK.into(),
        capabilities: pb::StorageCapabilities {
            shared_access: true,
            multi_node_attach: true,
            fencing: pb::Fencing::FENCING_LEASE_ONLY.into(),
            ..Default::default()
        }
        .into(),
        store: STORE.into(),
        ..Default::default()
    };
    vec![local, shared]
}

/// The example's declaration with its `data` volume in the shared class, or attaching `existing`.
fn on_shared(repo: &str, existing: Option<&str>) -> pb::IntentBody {
    let mut b = body(repo, &[]);
    b.volumes = vec![pb::VolumeSpec {
        name: "data".into(),
        storage_class: if existing.is_some() { String::new() } else { "shared".into() },
        existing: existing.map(str::to_owned),
        ..Default::default()
    }];
    b
}

const STOP: Option<pb::OnLeaseExpiry> = Some(pb::OnLeaseExpiry::ON_LEASE_EXPIRY_STOP);

#[tokio::test]
async fn a_volume_in_a_shared_store_is_reached_from_any_node_offering_it() {
    let h = Harness::new().await;
    let (a, b) = (TestAgent::enroll(&h, "fra-1", 1).await, TestAgent::enroll(&h, "fra-2", 2).await);
    let (mut sa, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], classes())).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(create("db", &project, "db", on_shared(&repo, None)));
    let db = declared.await.unwrap().into_owned().workload.into_option().unwrap().id;
    let created = until(&mut sa, assignment(|a| a.plan.is_set())).await;
    assert_eq!(created.on_lease_expiry.as_known(), STOP, "attachable from another node");
    let last = complete(&mut sa, &db, created.plan.as_option().unwrap(), 1).await;
    held_and_released(&mut sa, &db).await;
    let mut reported = ensured(&db, "data", false, false);
    reported.storage_class = "shared".into();
    send(&mut sa, vec![volume_record(last + 1, reported.clone())]).await;
    assert_eq!(until(&mut sa, ack).await, last + 1);
    let volumes = h.volumes(h.root());
    let listed = volumes.list_volumes(list(&project, false)).await.unwrap().into_owned().volumes;
    let data = listed.iter().find(|v| v.name == "data").unwrap().clone();
    assert_eq!(
        (data.node.as_deref(), data.store.as_str()),
        (None, STORE),
        "the store's, not a node's"
    );

    retired(&h, &mut sa, &db, last + 2).await;
    let (mut sb, first) =
        TestAgent::open(&h, b.hello_with(SystemTime::now(), &[2; 16], classes())).await;
    welcome(&first.unwrap());
    cordon(&h, &a.node).await;
    let adopting = on_shared(&repo, Some(&data.id));
    let adopting = workloads.declare_intent(create("app", &project, "app", adopting));
    let adoption = adopting.await.unwrap().into_owned();
    let plan = adoption.plan.as_option().unwrap();
    if plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
        h.plans(h.root()).approve_plan(approve("approve-app", &plan.id)).await.unwrap();
    }
    let app = adoption.workload.as_option().unwrap().id.clone();
    let told = until(&mut sb, assignment(|t| t.workload == app && t.plan.is_set())).await;
    assert_eq!(told.volume_ids.get("data"), Some(&format!("{db}.data")), "on the other node");
    assert_eq!(told.on_lease_expiry.as_known(), STOP);

    let last = complete(&mut sb, &app, told.plan.as_option().unwrap(), 1).await;
    held_and_released(&mut sb, &app).await;
    let again = pb_ensured(&app, &db);
    send(&mut sb, vec![volume_record(last + 1, again)]).await;
    assert_eq!(until(&mut sb, ack).await, last + 1);
    let listed = volumes.list_volumes(list(&project, false)).await.unwrap().into_owned().volumes;
    let shared: Vec<_> = listed.iter().filter(|v| v.store == STORE).collect();
    assert_eq!(shared.len(), 1, "one volume, whichever node reports it");
    assert_eq!(shared[0].attached_to.as_deref(), Some(app.as_str()));
}

/// The volume `db` made, ensured again by `app` on the other node.
fn pb_ensured(app: &str, db: &str) -> korpis_proto::korpis::agent::v1::VolumeEvent {
    let mut e = ensured(app, "data", false, false);
    e.helper_id = format!("{db}.data");
    e.storage_class = "shared".into();
    e
}

#[tokio::test]
async fn a_store_belongs_to_the_organization_whose_node_declared_it_first() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-1", 1).await;
    let (_sa, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], classes())).await;
    welcome(&first.unwrap());
    let made = pb::CreateOrganizationRequest {
        mutation: crate::support::mutation("org-tenant").into(),
        name: "tenant".into(),
        parent: Some(h.root_org()),
        ..Default::default()
    };
    let made = h.orgs(h.root()).create_organization(made).await.unwrap().into_owned();
    let tenant = made.organization.into_option().unwrap().id;
    let c = TestAgent::enroll_into(&h, &tenant, "ams-1", 3).await;
    let (_sc, first) =
        TestAgent::open(&h, c.hello_with(SystemTime::now(), &[3; 16], classes())).await;
    welcome(&first.unwrap());

    let shown = |node: &str| {
        let request = pb::GetNodeRequest { id: node.to_owned(), ..Default::default() };
        let nodes = h.nodes(h.root());
        async move {
            let got = nodes.get_node(request).await.unwrap().into_owned().node.into_option();
            let classes = got.unwrap().storage_classes;
            classes.into_iter().find(|c| c.name == "shared").unwrap().store
        }
    };
    assert_eq!(shown(&a.node).await, STORE, "the first node's organization holds it");
    assert_eq!(shown(&c.node).await, "", "another organization's node is not given it");
    assert_eq!(h.effects("node.store_refused").await.len(), 1, "and that is recorded");
}
