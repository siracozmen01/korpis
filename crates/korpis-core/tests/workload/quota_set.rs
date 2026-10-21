// SPDX-License-Identifier: AGPL-3.0-or-later

//! A quota set as a parent sets it (§5.1 of docs/05-scheduling.md, §3.1 of docs/01-model.md):
//! one entry of each shape, no reservation above its limit, overcommit only where it is
//! possible; what children are given sums within the parent's reservations, never
//! oversubscribed, and within its limits times the ratio it permits; a project's sub-limit fits
//! its organization's.

use crate::support::GIB;
use korpis_core::labels::LabelSelector;
use korpis_core::tenancy::quota::{
    Bounds, Dimension as D, QuotaEntry, QuotaOwner, QuotaSet, SetError,
};
use std::collections::BTreeMap;
use uuid::Uuid;

fn entry(reservations: &[(D, u64)], limits: &[(D, u64)]) -> QuotaEntry {
    QuotaEntry {
        id: Uuid::now_v7(),
        owner: QuotaOwner::Organization(Uuid::now_v7()),
        selector: None,
        nodes: None,
        reservations: reservations.iter().copied().collect::<Bounds>(),
        limits: limits.iter().copied().collect::<Bounds>(),
        version: 1,
        egress: None,
        held: None,
    }
}

fn set(entries: Vec<QuotaEntry>, overcommit: &[(D, u32)]) -> QuotaSet {
    QuotaSet { entries, child_overcommit: overcommit.iter().copied().collect() }
}

#[test]
fn a_set_says_each_shape_once_reserves_within_its_limits_and_oversells_only_what_can_be() {
    let one = entry(&[(D::Memory, GIB)], &[]);
    assert_eq!(set(vec![one.clone(), one], &[]).validate(), Err(SetError::Repeated(1)));
    let mut eu = entry(&[], &[]);
    eu.nodes = Some(LabelSelector {
        match_labels: BTreeMap::from([("region".into(), "eu".into())]),
        expressions: Vec::new(),
    });
    set(vec![entry(&[], &[]), eu], &[]).validate().unwrap();
    let above = set(vec![entry(&[(D::Memory, 2 * GIB)], &[(D::Memory, GIB)])], &[]);
    assert!(matches!(above.validate(), Err(SetError::ReservedAboveLimit { .. })));
    let ipv4 = set(vec![], &[(D::DedicatedIpv4, 200)]);
    assert!(matches!(ipv4.validate(), Err(SetError::Overcommit { .. })), "exclusive by nature");
    assert!(set(vec![], &[(D::Cpu, 50)]).validate().is_err(), "below one hundred percent");
}

#[test]
fn reservations_given_below_sum_within_the_reservations_above_and_limits_oversell_by_the_ratio() {
    // A reseller guarantees 32 GiB and has sold 32 GiB, overselling memory four times over.
    let reseller =
        set(vec![entry(&[(D::Memory, 32 * GIB)], &[(D::Memory, 32 * GIB)])], &[(D::Memory, 400)]);
    let customer = set(vec![entry(&[(D::Memory, 4 * GIB)], &[(D::Memory, 16 * GIB)])], &[]);
    let eight = vec![&customer; 8];
    reseller.fits(&eight).unwrap();
    let nine = vec![&customer; 9];
    assert!(
        matches!(reseller.fits(&nine), Err(SetError::Overallocated { side: "reservations", .. })),
        "36 GiB guaranteed of 32"
    );
    let greedy = set(vec![entry(&[], &[(D::Memory, 64 * GIB)])], &[]);
    let three = vec![&greedy, &greedy, &greedy];
    assert!(
        matches!(reseller.fits(&three), Err(SetError::Overallocated { side: "limits", .. })),
        "192 GiB sold of 128"
    );
    let unenabled = set(vec![entry(&[], &[(D::Memory, 32 * GIB)])], &[]);
    let two = vec![&customer, &customer, &customer];
    assert!(unenabled.fits(&two).is_err(), "a parent that enabled no overcommit sells none");
}

#[test]
fn a_project_sub_limit_fits_its_organization_and_leaves_the_rest_to_it() {
    let org = set(vec![entry(&[(D::Memory, 8 * GIB)], &[(D::Workloads, 10)])], &[]);
    let within = set(vec![entry(&[(D::Memory, 4 * GIB)], &[])], &[]);
    org.bounds_project(&within).unwrap();
    let beyond = set(vec![entry(&[], &[(D::Workloads, 11)])], &[]);
    assert!(org.bounds_project(&beyond).is_err());
}
