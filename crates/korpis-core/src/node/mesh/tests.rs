// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;
use std::time::UNIX_EPOCH;

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

fn candidate(n: u128, enrolled: u64, pool: &str, address: Option<&str>) -> Candidate {
    Candidate {
        node: Uuid::from_u128(n),
        enrolled_at: at(enrolled),
        addresses: address.iter().map(|a| a.parse().unwrap()).collect(),
        facts: OverlayFacts {
            key: [u8::try_from(n).unwrap(); 32],
            port: 51820,
            pools: vec![pool.parse().unwrap()],
            handshakes: BTreeMap::new(),
        },
    }
}

#[test]
fn a_node_whose_pool_overlaps_an_earlier_member_s_is_outside_and_says_with_which() {
    let m = mesh(vec![
        candidate(2, 20, "10.47.0.0/16", None),
        candidate(1, 10, "10.47.0.0/16", Some("203.0.113.1")),
        candidate(3, 30, "10.48.0.0/22", Some("203.0.113.3")),
    ]);
    let members: Vec<u128> = m.members.iter().map(|x| x.node.as_u128()).collect();
    assert_eq!(members, [1, 3], "the earlier of the two old defaults stays");
    assert_eq!(m.members[0].endpoint, Some("203.0.113.1:51820".parse().unwrap()));
    assert_eq!(m.outside.len(), 1);
    assert!(m.outside[0].1.contains(&Uuid::from_u128(1).to_string()), "{:?}", m.outside);
}

#[test]
fn a_pair_is_reachable_only_when_both_report_a_handshake_younger_than_reject_after() {
    let (mut a, mut b) =
        (candidate(1, 0, "10.48.0.0/22", None), candidate(2, 0, "10.48.4.0/22", None));
    let now = at(1_000);
    a.facts.handshakes.insert(b.facts.key, at(900));
    assert!(!reachable(&a.facts, &b.facts, now), "one side's word is not enough");
    b.facts.handshakes.insert(a.facts.key, at(950));
    assert!(reachable(&a.facts, &b.facts, now));
    assert!(!reachable(&a.facts, &b.facts, at(1_081)), "A's is past Reject-After-Time");
}

#[test]
fn the_pairs_are_the_members_reaching_each_other_either_way_round() {
    let (mut a, mut b) =
        (candidate(1, 0, "10.48.0.0/22", None), candidate(2, 0, "10.48.4.0/22", None));
    let c = candidate(3, 0, "10.48.8.0/22", None);
    a.facts.handshakes.insert(b.facts.key, at(990));
    b.facts.handshakes.insert(a.facts.key, at(990));
    let all = vec![a, b, c];
    let pairs = Pairs::of(&mesh(all.clone()), &all, at(1_000));
    let (one, two, three) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
    assert!(pairs.joins(one, two) && pairs.joins(two, one));
    assert!(!pairs.joins(one, three), "no handshake with C");
}

#[test]
fn a_node_stands_available_when_it_reaches_every_other_member() {
    let (mut a, mut b) =
        (candidate(1, 0, "10.48.0.0/22", None), candidate(2, 0, "10.48.4.0/22", None));
    let now = at(1_000);
    let all = vec![a.clone(), b.clone()];
    let one = Uuid::from_u128(1);
    let standing = Standing::of(one, (&mesh(all.clone()), &all), now);
    assert!(!standing.available, "no handshake yet");
    assert_eq!(standing.peers, [(Uuid::from_u128(2), false)]);
    a.facts.handshakes.insert(b.facts.key, at(990));
    b.facts.handshakes.insert(a.facts.key, at(990));
    let all = vec![a, b];
    let standing = Standing::of(one, (&mesh(all.clone()), &all), now);
    assert!(standing.available && standing.facts.is_some());
    let lone = vec![candidate(3, 0, "10.48.8.0/22", None)];
    assert!(Standing::of(Uuid::from_u128(3), (&mesh(lone.clone()), &lone), now).available);
    let nowhere = Standing::of(Uuid::from_u128(9), (&mesh(all.clone()), &all), now);
    assert_eq!(nowhere, Standing::default(), "not a member, reporting nothing");
}
