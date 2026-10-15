// SPDX-License-Identifier: AGPL-3.0-or-later

//! The signals scored from what the control plane learns of nodes over time (§2.2 of
//! docs/05-scheduling.md): content locality decides between nodes whose utilization is close,
//! and never overrides the strategy.

use super::{Case, GIB, node};
use crate::schedule::{Signal, Strategy};
use crate::tenancy::quota::{Bounds, Dimension};
use uuid::Uuid;

#[test]
fn content_locality_decides_a_near_tie_and_never_overrides_the_strategy() {
    let (one, two) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let mut case = Case::new(vec![node(1, &[]), node(2, &[])]);
    case.policy.strategy = Strategy::Distributed;
    case.content.insert(two, true);
    case.content.insert(one, false);
    let e = case.run();
    assert_eq!(e.chosen, Some(two), "alike otherwise, the node holding the recipe is preferred");
    let locality = |n| e.scored.iter().find(|s| s.node == n && s.signal == Signal::ContentLocality);
    assert_eq!(locality(two).map(|s| (s.value, s.weight)), Some((1.0, 0.25)));

    // Three quarters of its memory held moves its mean utilization by more than a quarter
    // (the mean is over memory and CPU): the strategy wins.
    case.allocated.insert(two, Bounds::from([(Dimension::Memory, 6 * GIB)]));
    assert_eq!(case.run().chosen, Some(one), "distributed still prefers the emptier node");

    // A node that never reported is not scored for it at all.
    case.content.remove(&one);
    let e = case.run();
    assert!(!e.scored.iter().any(|s| s.node == one && s.signal == Signal::ContentLocality));
}
