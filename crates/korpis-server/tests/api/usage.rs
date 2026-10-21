// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading metering (§2 of docs/15-observability.md): an organization's usage under
//! `metering.read`, a page at a time, without the operator's node; refused without the action;
//! and the days of metering with where each stands.

use crate::agent::{TestAgent, welcome};
use crate::journal::{ack, assignment, send, until};
use crate::metering::sample_at;
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use korpis_core::metering::{DAY, day_of};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::principal;
use std::time::{Duration, SystemTime};

const HOUR: Duration = Duration::from_secs(3600);

/// A token holding `actions` on `organization`; its secret.
async fn holding(h: &Harness, organization: &str, actions: &[&str]) -> String {
    let (token, secret) = h.bare_token().await;
    let issue = pb::IssueGrantRequest {
        mutation: mutation(&format!("to-{token}")).into(),
        parent: h.root_grant(),
        subject: principal::to_pb(&Principal::Token(token)).into(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        scope: org_scope(organization).into(),
        ..Default::default()
    };
    h.grants(h.root()).issue_grant(issue).await.unwrap();
    secret.expose().to_owned()
}

fn asked(organization: &str, from: SystemTime, size: u32, token: &str) -> pb::ListUsageRequest {
    pb::ListUsageRequest {
        organization: Some(organization.to_owned()),
        from: korpis_wire::timestamp(from).into(),
        page: pb::PageRequest { page_size: size, page_token: token.into(), ..Default::default() }
            .into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn an_organization_reads_what_its_workloads_used_and_which_days_closed() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-9", 6).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[6; 16])).await;
    welcome(&first.unwrap());
    assert_eq!(until(&mut s, ack).await, 0);
    let made = pb::CreateOrganizationRequest {
        mutation: mutation("org-blockhaven").into(),
        name: "blockhaven".into(),
        parent: Some(h.root_org()),
        ..Default::default()
    };
    let made = h.orgs(h.root()).create_organization(made).await.unwrap().into_owned();
    let tenant = made.organization.into_option().unwrap().id;
    let survival = project(&h, &tenant, "survival").await;
    let declared = create("w", &survival, "mc", body(&push_paper().await, &[]));
    let declared = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    until(&mut s, assignment(|a| a.workload == workload)).await;
    let start = day_of(SystemTime::now()) - DAY;
    let samples =
        vec![sample_at(1, &workload, start, 100), sample_at(2, &workload, start + HOUR, 250)];
    send(&mut s, samples).await;
    assert_eq!(until(&mut s, ack).await, 2);

    // A page at a time, in the order the intervals begin, without the operator's node.
    let reader = holding(&h, &tenant, &["metering.read"]).await;
    let meter = h.metering(h.config(Some(&reader)));
    let one = meter.list_usage(asked(&tenant, start, 1, "")).await.unwrap().into_owned();
    assert_eq!(one.records.len(), 1);
    assert_eq!((one.records[0].cpu_usec, one.records[0].node.clone()), (Some(100), None));
    let token = one.page.into_option().unwrap().next_page_token;
    let two = meter.list_usage(asked(&tenant, start, 1, &token)).await.unwrap().into_owned();
    assert_eq!(two.records[0].cpu_usec, Some(150), "the second interval, its difference");
    let as_root = h.metering(h.root()).list_usage(asked(&tenant, start, 10, "")).await.unwrap();
    assert!(
        as_root.into_owned().records.iter().all(|r| r.node.is_some()),
        "the operator sees where"
    );

    // Reading the organization is not reading what it used.
    let other = holding(&h, &tenant, &["org.read", "workload.read"]).await;
    let refused = h.metering(h.config(Some(&other))).list_usage(asked(&tenant, start, 1, "")).await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::PermissionDenied);

    // The days: yesterday's is closing until its lateness passes, and today's is open.
    let days =
        pb::ListPeriodsRequest { from: korpis_wire::timestamp(start).into(), ..Default::default() };
    let periods = meter.list_periods(days).await.unwrap().into_owned().periods;
    let states: Vec<_> = periods.iter().map(|p| p.state.as_known()).collect();
    assert_eq!(
        states,
        [Some(pb::PeriodState::PERIOD_STATE_CLOSING), Some(pb::PeriodState::PERIOD_STATE_OPEN)]
    );
}
