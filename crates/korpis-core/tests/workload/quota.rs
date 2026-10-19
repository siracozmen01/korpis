// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota held at admission (§5.1 of docs/05-scheduling.md).

use crate::support::{GIB, body};
use korpis_core::labels::LabelSelector;
use korpis_core::tenancy::quota::{
    Bounds, Dimension as D, Held, QuotaEntry, QuotaOwner, Side, Totals,
};
use std::collections::BTreeMap;
use uuid::Uuid;

fn entry(reservations: Bounds, limits: Bounds) -> QuotaEntry {
    QuotaEntry {
        id: Uuid::now_v7(),
        owner: QuotaOwner::Project(Uuid::now_v7()),
        selector: None,
        reservations,
        limits,
        version: 1,
        held: None,
    }
}

#[test]
fn a_workload_holds_its_reservations_its_volumes_and_itself() {
    let held = Held::of(&body());
    assert_eq!(held.reservations[&D::Memory], 4 * GIB);
    assert_eq!(held.limits[&D::Memory], Some(6 * GIB));
    let stream = body().streams.disk_bound();
    assert_eq!(stream, 72 * 1024 * 1024);
    assert_eq!(
        held.reservations[&D::Disk],
        40 * GIB + stream,
        "the volume it creates, and what its console stream can keep, are disk it holds"
    );
    let quiet = korpis_core::workload::IntentBody {
        console: korpis_core::workload::spec::ConsoleMode::None,
        ..body()
    };
    assert_eq!(Held::of(&quiet).reservations[&D::Disk], 40 * GIB, "no console, no stream");
    // A task's runs each write a console of their own: one stream for each run its node can
    // keep at once, the kept and the active.
    let policy = korpis_core::workload::spec::RunPolicy {
        keep_successful: 2,
        keep_failed: 3,
        ..Default::default()
    };
    let task = korpis_core::workload::IntentBody {
        lifecycle: korpis_core::workload::spec::Lifecycle::Task,
        runs: Some(policy),
        ..body()
    };
    assert_eq!(Held::of(&task).reservations[&D::Disk], 40 * GIB + (2 + 3 + 1) * stream);
    assert_eq!(held.reservations[&D::Workloads], 1);
    assert_eq!(held.reservations[&D::Endpoints], 1);
    assert_eq!(held.limits[&D::DiskReadBytes], None, "no I/O ceiling declared is unbounded");
}

#[test]
fn growth_past_a_bound_is_refused_and_naming_the_dimension() {
    let held = Held::of(&body());
    let e = entry(Bounds::from([(D::Memory, 8 * GIB)]), Bounds::new());
    e.admit(&Totals::sum([&held]), None, &held).unwrap();
    let three = [held.clone(), held.clone()];
    let refused = e.admit(&Totals::sum(&three), None, &held).unwrap_err();
    assert_eq!(
        (refused.dimension, refused.side, refused.would),
        (D::Memory, Side::Reservation, 12 * GIB)
    );
}

#[test]
fn a_change_that_does_not_grow_passes_even_over_a_lowered_bound() {
    let held = Held::of(&body());
    let e = entry(Bounds::from([(D::Memory, GIB)]), Bounds::new());
    e.admit(&Totals::default(), Some(&held), &held).unwrap();
    assert!(
        e.admit(&Totals::default(), None, &held).is_err(),
        "but a new workload is not admitted"
    );
}

#[test]
fn an_unbounded_limit_cannot_fit_a_bounded_entry() {
    let held = Held::of(&body());
    let e = entry(Bounds::new(), Bounds::from([(D::DiskReadBytes, 100 << 20)]));
    let refused = e.admit(&Totals::default(), None, &held).unwrap_err();
    assert_eq!((refused.side, refused.would), (Side::Limit, u64::MAX));
}

#[test]
fn a_selector_counts_only_the_workloads_it_matches() {
    let mut e = entry(Bounds::new(), Bounds::new());
    e.selector = Some(LabelSelector {
        match_labels: BTreeMap::from([("tier".into(), "game".into())]),
        expressions: Vec::new(),
    });
    assert!(e.counts(&BTreeMap::from([("tier".into(), "game".into())])));
    assert!(!e.counts(&BTreeMap::new()));
}

#[test]
fn totals_take_back_exactly_what_they_counted_an_unbounded_limit_included() {
    let held = Held::of(&body());
    let mut totals = Totals::sum([&held, &held]);
    assert_eq!(totals.reserved(D::Memory), 8 * GIB);
    assert_eq!(totals.limited(D::Memory), Some(12 * GIB));
    assert_eq!(totals.limited(D::DiskReadBytes), None, "one unbounded limit, the sum is too");
    totals.sub(&held);
    assert_eq!(totals, Totals::sum([&held]), "one taken back");
    totals.sub(&held);
    assert_eq!(totals, Totals::default(), "and nothing left, not even a zero");
    assert_eq!(totals.limited(D::DiskReadBytes), Some(0), "nothing unbounded is counted");
}
