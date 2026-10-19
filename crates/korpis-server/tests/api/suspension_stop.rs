// SPDX-License-Identifier: AGPL-3.0-or-later

//! A suspension that stops the workloads (§2.1 of docs/08-identity.md), on a real control plane
//! and a node: every workload beneath the suspended organization is told stopped whatever power
//! it declares, a plan waiting to apply waits on, and one already applying is let finish; a
//! nested suspension holds its own subtree until it is itself resumed; nothing is written onto
//! any workload, so resuming returns each to its own power; and suspending with the workloads
//! stopped, or resuming them, takes the authority to stop or to start them.

use crate::agent::{Stream, TestAgent, next, welcome};
use crate::delegation::{Holder, delegate, delegation, organization};
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation, org_scope};
use buffa::Message as _;
use connectrpc::ErrorCode;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;
use uuid::Uuid;

fn suspend(org: &str, key: &str, stop: bool) -> pb::SuspendOrganizationRequest {
    let on_suspend =
        if stop { pb::OnSuspend::ON_SUSPEND_STOP } else { pb::OnSuspend::ON_SUSPEND_KEEP_RUNNING };
    pb::SuspendOrganizationRequest {
        mutation: mutation(key).into(),
        organization: org.to_owned(),
        reason: "the invoice of October is unpaid".into(),
        on_suspend: on_suspend.into(),
        ..Default::default()
    }
}

fn resume(org: &str, key: &str) -> pb::ResumeOrganizationRequest {
    pb::ResumeOrganizationRequest {
        mutation: mutation(key).into(),
        organization: org.to_owned(),
        ..Default::default()
    }
}

/// The host, holding the root grant.
fn host(h: &Harness) -> Holder {
    Holder { grant: h.root_grant(), config: h.root(), token: Uuid::nil() }
}

async fn session(h: &Harness) -> Stream {
    let a = TestAgent::enroll(h, "fra-1", 1).await;
    let (s, first) = TestAgent::open(h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    s
}

/// A workload named `name` declared in `project`; its wire id.
async fn declared(h: &Harness, project: &str, name: &str) -> String {
    let declare = create(&format!("d-{name}"), project, name, body(&push_paper().await, &[]));
    let out = h.workloads(h.root()).declare_intent(declare).await.unwrap().into_owned();
    out.workload.as_option().unwrap().id.clone()
}

/// The assignment of each of `workloads` in the next set in which `pick` accepts them all.
async fn told(
    s: &mut Stream,
    workloads: &[&str],
    pick: impl Fn(&agent::Assignment) -> bool,
) -> Vec<agent::Assignment> {
    for _ in 0..30 {
        let Some(Down::AssignmentSet(set)) = next(s).await.message else { continue };
        let set: Vec<agent::Assignment> = set
            .assignments
            .iter()
            .map(|sealed| agent::Assignment::decode_from_slice(&sealed.assignment).unwrap())
            .collect();
        let found: Vec<agent::Assignment> = workloads
            .iter()
            .filter_map(|w| set.iter().find(|a| a.workload == *w && pick(a)).cloned())
            .collect();
        if found.len() == workloads.len() {
            return found;
        }
    }
    panic!("{workloads:?} were not told so within thirty messages");
}

fn power(a: &agent::Assignment) -> Option<pb::PowerState> {
    a.power.as_option().and_then(|p| p.desired.as_known())
}

fn stopped(a: &agent::Assignment) -> bool {
    power(a) == Some(pb::PowerState::POWER_STATE_STOPPED)
}

fn running(a: &agent::Assignment) -> bool {
    power(a) == Some(pb::PowerState::POWER_STATE_RUNNING)
}

/// The status of the workload with wire id `id`, as the host reads it.
async fn status(h: &Harness, id: &str) -> pb::WorkloadStatus {
    let get = pb::GetWorkloadRequest { id: id.to_owned(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    got.status.into_option().unwrap()
}

#[tokio::test]
async fn a_suspension_that_stops_holds_every_workload_beneath_until_it_is_resumed() {
    let h = Harness::new().await;
    let mut s = session(&h).await;
    let customer = organization(&h, &host(&h), &h.root_org(), "customer").await;
    let project = project(&h, &customer, "games").await;
    let served = declared(&h, &project, "served").await;
    told(&mut s, &[&served], |a| a.plan.is_set() && running(a)).await;

    let orgs = h.orgs(h.root());
    orgs.suspend_organization(suspend(&customer, "s1", true)).await.unwrap();
    let held = told(&mut s, &[&served], stopped).await;
    assert!(held[0].plan.is_set(), "the plan applying is let finish");
    let shown = status(&h, &served).await;
    assert!(shown.held_by_suspension, "and its status says why");
    let declared_power = shown.workload.into_option().unwrap().power.into_option().unwrap();
    assert_eq!(
        declared_power.desired.as_known(),
        Some(pb::PowerState::POWER_STATE_RUNNING),
        "nothing was written onto it"
    );
    assert!(h.effects("workload.stop").await.is_empty(), "and no stop declared for it");
    let later = declared(&h, &project, "later").await;
    let waiting = told(&mut s, &[&later], |_| true).await;
    assert!(stopped(&waiting[0]) && !waiting[0].plan.is_set(), "one made meanwhile waits");

    orgs.resume_organization(resume(&customer, "r1")).await.unwrap();
    told(&mut s, &[&served, &later], |a| a.plan.is_set() && running(a)).await;
    assert!(!status(&h, &served).await.held_by_suspension, "resumed as it was");
}

#[tokio::test]
async fn resuming_an_ancestor_leaves_a_suspended_descendant_held() {
    let h = Harness::new().await;
    let mut s = session(&h).await;
    let customer = organization(&h, &host(&h), &h.root_org(), "customer").await;
    let sub = organization(&h, &host(&h), &customer, "sub").await;
    let project = project(&h, &sub, "games").await;
    let served = declared(&h, &project, "served").await;
    told(&mut s, &[&served], |a| a.plan.is_set() && running(a)).await;

    let orgs = h.orgs(h.root());
    orgs.suspend_organization(suspend(&customer, "s1", true)).await.unwrap();
    told(&mut s, &[&served], stopped).await;
    orgs.suspend_organization(suspend(&sub, "s2", true)).await.unwrap();
    orgs.resume_organization(resume(&customer, "r1")).await.unwrap();
    assert!(status(&h, &served).await.held_by_suspension, "its own suspension still holds it");
    orgs.resume_organization(resume(&sub, "r2")).await.unwrap();
    told(&mut s, &[&served], running).await;
    assert!(!status(&h, &served).await.held_by_suspension);
}

#[tokio::test]
async fn stopping_the_workloads_of_a_suspension_takes_the_authority_to_stop_and_start_them() {
    let h = Harness::new().await;
    let customer = organization(&h, &host(&h), &h.root_org(), "customer").await;
    let acts = ["org.suspend", "org.resume", "org.read"];
    let ask = (&acts[..], org_scope(&h.root_org()));
    let billing = delegate(&h, (&h.root_grant(), h.root()), ask, delegation(None, &[])).await;
    let orgs = h.orgs(billing.config.clone());

    let refused = orgs.suspend_organization(suspend(&customer, "s1", true)).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied, "stopping them takes workload.stop");
    orgs.suspend_organization(suspend(&customer, "s2", false)).await.unwrap();
    orgs.resume_organization(resume(&customer, "r1")).await.unwrap();

    h.orgs(h.root()).suspend_organization(suspend(&customer, "s3", true)).await.unwrap();
    let refused = orgs.resume_organization(resume(&customer, "r2")).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied, "starting them takes workload.start");
}
