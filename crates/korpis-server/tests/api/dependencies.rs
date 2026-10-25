// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dependencies, the control plane's half (§5 of docs/07-networking.md): a target is checked
//! when declared, resolved to the endpoint it reaches into the dependent's assignment, and a
//! dependent is placed on its target's node where no mesh joins two; a required dependency with
//! nothing to reach leaves it unplaced, saying so; and the dependent's status says what each
//! dependency reaches and how, and what it names and leaves out, with why.

use crate::agent::{Stream, TestAgent, welcome};
use crate::egress::assigned;
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, detail};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::dependency::Target;
use std::time::SystemTime;

/// A node of this test with its session open.
async fn node(h: &Harness, name: &str, seed: u8) -> (String, Stream) {
    let a = TestAgent::enroll(h, name, seed).await;
    let (s, first) = TestAgent::open(h, a.hello_at(SystemTime::now(), &[seed; 16])).await;
    welcome(&first.unwrap());
    (a.node.clone(), s)
}

/// Paper with only its `game` endpoint reachable: the others declared `none`.
pub(crate) fn one_endpoint(repo: &str) -> pb::IntentBody {
    let none = |name: &str| pb::EndpointSpec {
        name: name.into(),
        exposure: pb::Exposure::EXPOSURE_NONE.into(),
        ..Default::default()
    };
    pb::IntentBody { endpoints: vec![none("query"), none("rcon")], ..body(repo, &[]) }
}

pub(crate) fn on(target: Target, required: bool) -> pb::Dependency {
    pb::Dependency { name: "db".into(), target: Some(target), required, ..Default::default() }
}

pub(crate) async fn declare(
    h: &Harness,
    project: &str,
    name: &str,
    b: pb::IntentBody,
) -> pb::Workload {
    let d = create(name, project, name, b);
    let out = h.workloads(h.root()).declare_intent(d).await.unwrap().into_owned();
    out.workload.into_option().unwrap()
}

pub(crate) async fn placement(h: &Harness, workload: &str) -> pb::Placement {
    let get = pb::GetWorkloadRequest { id: workload.to_owned(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    got.status.into_option().unwrap().placement.into_option().unwrap_or_default()
}

#[tokio::test]
async fn a_dependent_is_placed_beside_its_target_and_told_the_endpoint_it_reaches() {
    let h = Harness::new().await;
    let ((one, _s1), (two, _s2)) = (node(&h, "fra-1", 1).await, node(&h, "fra-2", 2).await);
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "web").await;
    let db = declare(&h, &project, "db", one_endpoint(&repo)).await;
    let db_node = placement(&h, &db.id).await.node;
    assert!(db_node == one || db_node == two, "{db_node}");

    let mut web = one_endpoint(&repo);
    web.dependencies = vec![on(Target::Workload(db.id.clone()), true)];
    let web = declare(&h, &project, "web", web).await;
    assert_eq!(placement(&h, &web.id).await.node, db_node, "beside its database");
    let told = assigned(&h, &web.id).await;
    let d = &told.dependencies[0];
    assert_eq!((d.name.as_str(), d.required, d.targets.len()), ("db", true, 1));
    let t = &d.targets[0];
    assert_eq!(
        (t.workload.as_str(), t.endpoint.as_str(), t.container_port),
        (db.id.as_str(), "game", 25565)
    );
}

#[tokio::test]
async fn a_target_that_is_not_one_is_refused_and_one_with_nothing_to_reach_holds_it_back() {
    let h = Harness::new().await;
    let (_n, _s) = node(&h, "fra-1", 1).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "web").await;
    let other = project_named(&h, "elsewhere").await;
    let three = declare(&h, &project, "three", body(&repo, &[])).await;
    let away = declare(&h, &other, "away", one_endpoint(&repo)).await;
    let refused = |target: Target, key: &'static str| {
        let mut b = one_endpoint(&repo);
        b.dependencies = vec![on(target, false)];
        let d = create(key, &project, key, b);
        let workloads = h.workloads(h.root());
        async move { workloads.declare_intent(d).await.unwrap_err() }
    };
    let several = refused(Target::Workload(three.id.clone()), "w1").await;
    assert!(detail(&several).message.contains("several endpoints"), "{several:?}");
    let foreign = refused(Target::Workload(away.id.clone()), "w2").await;
    assert_eq!(detail(&foreign).field, "body.dependencies[0].target", "{foreign:?}");

    let mut nothing = one_endpoint(&repo);
    let selector = pb::LabelSelector {
        match_labels: [("tier".to_owned(), "none".to_owned())].into(),
        ..Default::default()
    };
    nothing.dependencies = vec![on(Target::Selector(Box::new(selector)), true)];
    let held = declare(&h, &project, "held", nothing).await;
    let get = pb::GetWorkloadRequest { id: held.id.clone(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    let status = got.status.into_option().unwrap();
    let why = status.unplaced.as_option().unwrap().explanation.as_option().unwrap();
    assert_eq!(why.filtered[0].check, "dependency", "{why:?}");
    assert!(why.filtered[0].detail.contains("required dependency db is not placed"));
}

/// A workload of `project` labelled `tier=db`.
async fn labelled(h: &Harness, project: &str, name: &str, b: pb::IntentBody) -> pb::Workload {
    let mut d = create(name, project, name, b);
    if let Some(korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target::Create(
        new,
    )) = d.target.as_mut()
    {
        new.labels = [("tier".to_owned(), "db".to_owned())].into();
    }
    let out = h.workloads(h.root()).declare_intent(d).await.unwrap().into_owned();
    out.workload.into_option().unwrap()
}

#[tokio::test]
async fn a_dependent_s_status_says_what_each_dependency_reaches_and_what_it_leaves_out_and_why() {
    let h = Harness::new().await;
    let (_n, _s) = node(&h, "fra-1", 1).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "web").await;
    let db = labelled(&h, &project, "db", one_endpoint(&repo)).await;
    let three = labelled(&h, &project, "three", body(&repo, &[])).await;
    let mut web = one_endpoint(&repo);
    let selector = pb::LabelSelector {
        match_labels: [("tier".to_owned(), "db".to_owned())].into(),
        ..Default::default()
    };
    web.dependencies = vec![on(Target::Selector(Box::new(selector)), false)];
    let web = declare(&h, &project, "web", web).await;
    let get = pb::GetWorkloadRequest { id: web.id.clone(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    let status = got.status.into_option().unwrap();
    let d = &status.dependencies[0];
    assert_eq!(d.targets.len(), 1);
    assert_eq!(d.targets[0].workload, db.id);
    assert_eq!(d.targets[0].reach, pb::DependencyReach::DEPENDENCY_REACH_SAME_NODE);
    assert_eq!(d.left_out.len(), 1, "the match with several endpoints");
    assert_eq!(d.left_out[0].workload, three.id);
    assert!(d.left_out[0].reason.contains("several endpoints"), "{:?}", d.left_out);
}

async fn project_named(h: &Harness, name: &str) -> String {
    project(h, &h.root_org(), name).await
}
