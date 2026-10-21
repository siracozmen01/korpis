// SPDX-License-Identifier: AGPL-3.0-or-later

//! The egress budget (§5.1 of docs/05-scheduling.md, "The egress budget"), on a real control
//! plane: a budget is set with what its spending does, and refused without it; the month's egress
//! of the workloads it counts is reckoned from their records; a spent budget holds each of them,
//! and its node is told in the workload's next assignment; a budget raised lets them go.

use crate::agent::{TestAgent, welcome};
use crate::journal::{ack, assignment, send, until};
use crate::metering::sample_at;
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation};
use connectrpc::ErrorCode;
use korpis_core::metering::interval_of;
use korpis_core::tenancy::quota::month_of;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

const HOUR: Duration = Duration::from_secs(3600);
const FLOOR: u64 = 1_000_000;

/// A set of one entry for everything with a budget of `bytes`, held to a floor once spent, or
/// with nothing said of its spending.
fn budget(bytes: u64, floor: bool) -> pb::QuotaSet {
    let exhausted = pb::EgressExhaustion { floor_bits_per_sec: FLOOR, ..Default::default() };
    let entry = pb::QuotaEntry {
        egress_bytes_per_month: Some(bytes),
        egress_exhausted: floor.then_some(exhausted).into(),
        ..Default::default()
    };
    pb::QuotaSet { entries: vec![entry], ..Default::default() }
}

fn setting(key: &str, org: &str, quota: pb::QuotaSet) -> pb::SetQuotaRequest {
    pb::SetQuotaRequest {
        mutation: mutation(key).into(),
        organization: org.to_owned(),
        quota: quota.into(),
        ..Default::default()
    }
}

/// A sample of `workload` for the hour from `start`, its egress `egress` so far.
fn sent(seq: u64, workload: &str, start: SystemTime, egress: u64) -> agent::JournalRecord {
    let mut record = sample_at(seq, workload, start, seq * 100);
    if let Some(B::Metering(s)) = record.body.as_mut()
        && let Some(c) = s.counters.as_option_mut()
    {
        c.egress_bytes = Some(egress);
    }
    record
}

fn held(a: &agent::Assignment) -> Option<(bool, u64, bool)> {
    a.shaping
        .as_option()
        .map(|s| (s.egress_exhausted, s.egress_floor_bits_per_sec, s.egress_cut_off))
}

#[tokio::test]
async fn a_spent_budget_holds_its_workloads_to_its_floor_until_it_is_raised() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-14", 12).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[12; 16])).await;
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

    let silent = h.orgs(h.root()).set_quota(setting("q0", &tenant, budget(10_000, false))).await;
    assert_eq!(silent.unwrap_err().code, ErrorCode::InvalidArgument, "a budget that does nothing");
    h.orgs(h.root()).set_quota(setting("q1", &tenant, budget(10_000, true))).await.unwrap();

    let survival = project(&h, &tenant, "survival").await;
    let declared = create("w", &survival, "mc", body(&push_paper().await, &[]));
    let declared = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    let told = until(&mut s, assignment(|a| a.workload == workload)).await;
    assert_eq!(held(&told).map(|h| h.0), Some(false), "nothing spent, nothing held");

    // Two hours of this month, the second spending the budget.
    let now = SystemTime::now();
    let start = month_of(now).0.max(interval_of(now, HOUR) - HOUR * 2);
    send(&mut s, vec![sent(1, &workload, start, 6_000), sent(2, &workload, start + HOUR, 12_000)])
        .await;
    assert_eq!(until(&mut s, ack).await, 2);
    assert_eq!(h.api.app().keep_egress().await.unwrap(), 1, "one workload held");
    let told =
        until(&mut s, assignment(|a| a.workload == workload && held(a).is_some_and(|h| h.0))).await;
    assert_eq!(held(&told), Some((true, FLOOR, false)), "held to the budget's floor");
    let get = pb::GetOrganizationRequest { id: tenant.clone(), ..Default::default() };
    let org = h.orgs(h.root()).get_organization(get.clone()).await.unwrap().into_owned();
    let quota = org.organization.into_option().unwrap().quota.into_option().unwrap();
    let spent = quota.entries[0].egress_spent.as_option().cloned().unwrap();
    assert_eq!((spent.bytes, spent.exhausted), (12_000, true));

    // Raised past what was sent, it is spent no longer, and the workload is let go.
    h.orgs(h.root()).set_quota(setting("q2", &tenant, budget(1 << 30, true))).await.unwrap();
    assert_eq!(h.api.app().keep_egress().await.unwrap(), 1, "one workload let go");
    let told =
        until(&mut s, assignment(|a| a.workload == workload && held(a).is_some_and(|h| !h.0)))
            .await;
    assert_eq!(held(&told), Some((false, 0, false)));
    assert_eq!(h.api.app().keep_egress().await.unwrap(), 0, "and nothing changes again");
}
