// SPDX-License-Identifier: AGPL-3.0-or-later

//! Scoring signals (§2.2 of docs/05-scheduling.md): the strategy decides between a full node and
//! an empty one, and content locality decides between nodes whose utilization is close and never
//! overrides it.

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

#[test]
fn the_strategy_decides_between_a_full_node_and_an_empty_one() {
    let mut case = Case::new(vec![node(1, &[]), node(2, &[])]);
    case.allocated.insert(Uuid::from_u128(1), Bounds::from([(Dimension::Memory, 4 * GIB)]));
    case.policy.strategy = Strategy::Packed;
    assert_eq!(case.run().chosen, Some(Uuid::from_u128(1)));
    case.policy.strategy = Strategy::Distributed;
    assert_eq!(case.run().chosen, Some(Uuid::from_u128(2)));
    case.policy.strategy = Strategy::Balanced;
    let e = case.run();
    assert_eq!((e.chosen, e.runner_up), (Some(Uuid::from_u128(2)), Some(Uuid::from_u128(1))));
    assert!(e.scored.iter().any(|s| s.signal == Signal::Utilization));
    assert!(!e.scored.iter().any(|s| s.signal == Signal::ContentLocality), "not guessed");
}
