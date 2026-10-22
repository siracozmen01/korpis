// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a parent guaranteed beneath it is kept (§5.1 of docs/05-scheduling.md, Finding 139 of
//! docs/23-walkthroughs.md, trace 52 there): a reseller given 12 GiB that guaranteed its
//! sub-tenant 8 runs no more than the 4 left of its own; it gives no other child what it would
//! then have to take back from its own; and its sub-tenant reserves its whole 8, even after the
//! host lowers the reseller beneath what the two hold together. Each paper server reserves 4 GiB.

use crate::delegation::{Holder, organization};
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, detail, mutation, reason};
use korpis_proto::korpis::v1 as pb;
use uuid::Uuid;

const GIB: u64 = 1 << 30;

/// A set of one entry for everything, reserving `reserved` GiB of memory and limiting it to
/// `limit`.
fn memory(reserved: u64, limit: u64) -> pb::QuotaSet {
    let bounds = |gib: u64| pb::QuotaBounds { memory_bytes: Some(gib * GIB), ..Default::default() };
    let entry = pb::QuotaEntry {
        reservation_bounds: bounds(reserved).into(),
        limit_bounds: bounds(limit).into(),
        ..Default::default()
    };
    pb::QuotaSet { entries: vec![entry], ..Default::default() }
}

async fn give(
    h: &Harness,
    key: &str,
    org: &str,
    (reserved, limit): (u64, u64),
) -> Result<(), connectrpc::ConnectError> {
    let req = pb::SetQuotaRequest {
        mutation: mutation(key).into(),
        organization: org.to_owned(),
        quota: memory(reserved, limit).into(),
        ..Default::default()
    };
    h.orgs(h.root()).set_quota(req).await.map(|_| ())
}

async fn paper(h: &Harness, (repo, project): (&str, &str), name: &str) -> Result<(), String> {
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(create(name, project, name, body(repo, &[]))).await;
    declared.map(|_| ()).map_err(|e| format!("{}:{}", reason(&e), detail(&e).field))
}

#[tokio::test]
async fn a_reseller_keeps_what_it_guaranteed_its_sub_tenant_whatever_it_runs_itself() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let host = Holder { grant: h.root_grant(), config: h.root(), token: Uuid::nil() };
    let reseller = organization(&h, &host, &h.root_org(), "northwind").await;
    let sub = organization(&h, &host, &reseller, "kestrel").await;
    give(&h, "northwind", &reseller, (12, 12)).await.unwrap();
    give(&h, "kestrel", &sub, (8, 8)).await.unwrap();
    let own = project(&h, &reseller, "own").await;
    let shop = project(&h, &sub, "shop").await;

    paper(&h, (&repo, &own), "own-1").await.unwrap();
    let refused = paper(&h, (&repo, &own), "own-2").await;
    assert_eq!(refused.unwrap_err(), "quota.exceeded:memory_bytes", "8 of 12 are kestrel's");
    let other = organization(&h, &host, &reseller, "pinecone").await;
    let unkept = give(&h, "pinecone", &other, (4, 4)).await.unwrap_err();
    assert_eq!(reason(&unkept), "quota.overallocated", "it would take it back from its own");

    paper(&h, (&repo, &shop), "shop-1").await.unwrap();
    // The host lowers the reseller's reservations to what it guaranteed, beneath the 12 the two
    // hold together; what it sold, its limits, it leaves.
    give(&h, "lowered", &reseller, (8, 12)).await.unwrap();
    paper(&h, (&repo, &shop), "shop-2").await.expect("kestrel reserves its whole 8");
    let beyond = paper(&h, (&repo, &shop), "shop-3").await;
    assert_eq!(beyond.unwrap_err(), "quota.exceeded:memory_bytes", "and no more");
}
