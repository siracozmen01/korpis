// SPDX-License-Identifier: AGPL-3.0-or-later

//! Setting quota sets from above (§5.1 of docs/05-scheduling.md, §3.1 of docs/01-model.md), on
//! a real control plane: the host gives its reseller a set, which the reseller cannot raise
//! itself; what the reseller gives its customers fits what it was given, reservations never
//! oversubscribed and limits oversold by the ratio the host permitted; the host cannot lower the
//! reseller below what it gave; a bound of zero allows none; a project's sub-limit fits its
//! organization's; and an entry kept by shape keeps what it counts.

use crate::delegation::{Holder, delegate, delegation, organization};
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, detail, mutation, org_scope, reason};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use uuid::Uuid;

const GIB: u64 = 1 << 30;

fn host(h: &Harness) -> Holder {
    Holder { grant: h.root_grant(), config: h.root(), token: Uuid::nil() }
}

/// A set of one entry for everything, bounding memory, and permitting memory overcommit.
fn memory(reserved: u64, limit: u64, overcommit: Option<u32>) -> pb::QuotaSet {
    let entry = pb::QuotaEntry {
        reservation_bounds: pb::QuotaBounds { memory_bytes: Some(reserved), ..Default::default() }
            .into(),
        limit_bounds: pb::QuotaBounds { memory_bytes: Some(limit), ..Default::default() }.into(),
        ..Default::default()
    };
    let mut set = pb::QuotaSet { entries: vec![entry], ..Default::default() };
    if let Some(p) = overcommit {
        set.child_overcommit_percent.insert("memory_bytes".into(), p);
    }
    set
}

fn setting(key: &str, org: &str, quota: pb::QuotaSet) -> pb::SetQuotaRequest {
    pb::SetQuotaRequest {
        mutation: mutation(key).into(),
        organization: org.to_owned(),
        quota: quota.into(),
        ..Default::default()
    }
}

/// The host's reseller, given 32 GiB guaranteed and sold, memory oversold four times over, and
/// a token holding the reseller's own administration.
async fn reseller(h: &Harness) -> (String, Holder) {
    let org = organization(h, &host(h), &h.root_org(), "reseller").await;
    let given = setting("give", &org, memory(32 * GIB, 32 * GIB, Some(400)));
    let set = h.orgs(h.root()).set_quota(given).await.unwrap().into_owned();
    let shown = set.organization.into_option().unwrap().quota.into_option().unwrap();
    assert_eq!(shown.entries.len(), 1);
    let acts = ["org.create", "org.read", "org.quota.write", "project.create", "project.read"];
    let ask = (&acts[..], org_scope(&org));
    let holder = delegate(h, (&h.root_grant(), h.root()), ask, delegation(None, &[])).await;
    (org, holder)
}

#[tokio::test]
async fn a_reseller_gives_its_customers_what_fits_and_raises_nothing_of_its_own() {
    let h = Harness::new().await;
    let (org, r) = reseller(&h).await;
    let (orgs, hosts) = (h.orgs(r.config.clone()), h.orgs(h.root()));
    let own = orgs.set_quota(setting("raise", &org, memory(64 * GIB, 64 * GIB, None))).await;
    assert_eq!(own.unwrap_err().code, ErrorCode::NotFound, "nothing above it is there for it");
    let root = hosts.set_quota(setting("root", &h.root_org(), memory(GIB, GIB, None))).await;
    assert_eq!(root.unwrap_err().code, ErrorCode::InvalidArgument, "a root holds no quota");

    for n in 0..8 {
        let c = organization(&h, &r, &org, &format!("c{n}")).await;
        let give = setting(&format!("c{n}"), &c, memory(4 * GIB, 16 * GIB, None));
        orgs.set_quota(give).await.unwrap();
    }
    let ninth = organization(&h, &r, &org, "c8").await;
    let refused = orgs.set_quota(setting("c8", &ninth, memory(4 * GIB, 16 * GIB, None))).await;
    let refused = refused.unwrap_err();
    assert_eq!(reason(&refused), "quota.overallocated", "36 GiB guaranteed of 32: {refused:?}");
    let oversold = orgs.set_quota(setting("c8b", &ninth, memory(0, 16 * GIB, None))).await;
    assert_eq!(reason(&oversold.unwrap_err()), "quota.overallocated", "144 GiB sold of 128");
    orgs.set_quota(setting("c8c", &ninth, memory(0, 0, None))).await.unwrap();

    let lower = setting("lower", &org, memory(16 * GIB, 32 * GIB, Some(400)));
    let lowered = hosts.set_quota(lower).await.unwrap_err();
    assert_eq!(reason(&lowered), "quota.overallocated", "below what it gave");
}

#[tokio::test]
async fn a_bound_of_zero_allows_none_and_a_kept_entry_keeps_what_it_counts() {
    let h = Harness::new().await;
    let (org, r) = reseller(&h).await;
    let customer = organization(&h, &r, &org, "customer").await;
    let games = project(&h, &customer, "games").await;
    let none = pb::QuotaSet {
        entries: vec![pb::QuotaEntry {
            reservation_bounds: pb::QuotaBounds { workloads: Some(0), ..Default::default() }.into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let orgs = h.orgs(r.config.clone());
    orgs.set_quota(setting("none", &customer, none)).await.unwrap();
    let repo = push_paper().await;
    let workloads = h.workloads(h.root());
    let refused = workloads.declare_intent(create("d1", &games, "a", body(&repo, &[]))).await;
    assert_eq!(detail(&refused.unwrap_err()).field, "workloads", "zero is a bound, not absence");

    let one = pb::QuotaSet {
        entries: vec![pb::QuotaEntry {
            reservation_bounds: pb::QuotaBounds { workloads: Some(1), ..Default::default() }.into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let at_one = orgs.set_quota(setting("one", &customer, one.clone())).await.unwrap();
    let at_one = at_one.into_owned().organization.into_option().unwrap().quota.into_option();
    let before = at_one.unwrap().entries[0].version;
    workloads.declare_intent(create("d2", &games, "a", body(&repo, &[]))).await.unwrap();
    let mut two = one;
    two.entries[0].reservation_bounds =
        pb::QuotaBounds { workloads: Some(2), ..Default::default() }.into();
    let set = orgs.set_quota(setting("two", &customer, two)).await.unwrap().into_owned();
    let quota = set.organization.into_option().unwrap().quota.into_option().unwrap();
    let held = quota.entries[0].held.as_option().unwrap();
    assert_eq!(held.reservations.as_option().unwrap().workloads, Some(1), "kept by its shape");
    // The same entry, consumed by the admission and changed since: two versions later.
    assert_eq!(quota.entries[0].version, before + 2);

    // Two workloads guaranteed to the customer; a project of it cannot be guaranteed three.
    let three = pb::QuotaSet {
        entries: vec![pb::QuotaEntry {
            reservation_bounds: pb::QuotaBounds { workloads: Some(3), ..Default::default() }.into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let sub = pb::SetQuotaRequest { project: Some(games), ..setting("sub", &customer, three) };
    let refused = orgs.set_quota(sub).await.unwrap_err();
    assert_eq!(reason(&refused), "quota.overallocated", "a sub-limit fits its organization");
}

#[tokio::test]
async fn two_siblings_given_the_last_of_their_parent_at_once_are_given_it_once() {
    let h = Harness::new().await;
    let (org, r) = reseller(&h).await;
    let (a, b) = (organization(&h, &r, &org, "a").await, organization(&h, &r, &org, "b").await);
    let orgs = h.orgs(r.config.clone());
    let (one, two) = tokio::join!(
        orgs.set_quota(setting("a", &a, memory(20 * GIB, 20 * GIB, None))),
        orgs.set_quota(setting("b", &b, memory(20 * GIB, 20 * GIB, None))),
    );
    assert_eq!([one.is_ok(), two.is_ok()].iter().filter(|ok| **ok).count(), 1, "40 GiB of 32");
    let refused = one.err().or(two.err()).unwrap();
    assert_eq!(reason(&refused), "quota.overallocated");
}

#[tokio::test]
async fn whether_a_project_exists_is_told_only_to_who_may_set_its_quota() {
    let h = Harness::new().await;
    let (org, _r) = reseller(&h).await;
    let shop = project(&h, &org, "shop").await;
    let (_, secret) = h.bare_token().await;
    let stranger = h.orgs(h.config(Some(secret.expose())));
    let ask = |key: &str, project: String| pb::SetQuotaRequest {
        project: Some(project),
        ..setting(key, &org, memory(GIB, GIB, None))
    };
    let real = stranger.set_quota(ask("real", shop)).await.unwrap_err();
    let made_up = format!("prj_{}", Uuid::now_v7().simple());
    let none = stranger.set_quota(ask("none", made_up)).await.unwrap_err();
    assert_eq!((real.code, real.message.clone()), (none.code, none.message), "the same refusal");
}
