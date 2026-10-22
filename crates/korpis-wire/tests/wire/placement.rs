// SPDX-License-Identifier: AGPL-3.0-or-later

//! A placement policy from the wire (§2.2 of docs/05-scheduling.md), as an organization sets one
//! and the installation's flag gives one: its spread constraints and affinity rules are bounded,
//! since placement evaluates every one of them while it holds the installation's one lock
//! (Finding 143 of docs/23-walkthroughs.md).

use korpis_proto::korpis::v1 as pb;
use korpis_wire::placement::policy_from_pb;

fn policy(spread: usize, affinity: usize) -> pb::PlacementPolicy {
    let constraint = pb::SpreadConstraint {
        topology_key: "rack".into(),
        max_skew: 1,
        scope: pb::SpreadScope::SPREAD_SCOPE_PROJECT.into(),
        ..Default::default()
    };
    let rule = pb::AffinityRule { anti: true, topology_key: "rack".into(), ..Default::default() };
    pb::PlacementPolicy {
        spread: vec![constraint; spread],
        affinity: vec![rule; affinity],
        ..Default::default()
    }
}

#[test]
fn a_policy_states_at_most_sixteen_spread_constraints_and_thirty_two_affinity_rules() {
    let most = policy_from_pb("policy", &policy(16, 32)).unwrap();
    assert_eq!((most.spread.len(), most.affinity.len()), (16, 32));
    let spread = policy_from_pb("policy", &policy(17, 0)).unwrap_err();
    assert!(spread.to_string().contains("policy.spread"), "{spread}");
    let affinity = policy_from_pb("policy", &policy(0, 33)).unwrap_err();
    assert!(affinity.to_string().contains("policy.affinity"), "{affinity}");
}
