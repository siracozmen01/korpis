// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;
use crate::edge::probe::EdgeRoute;

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
}

fn hold(node: Uuid, epoch: u64) -> Hold {
    Hold {
        address: "198.51.100.2".parse().unwrap(),
        group: Uuid::nil(),
        node,
        epoch,
        term: Duration::from_secs(10),
        received_at: at(100),
        released_at: None,
    }
}

#[test]
fn a_ready_holder_keeps_its_address_and_a_failing_one_loses_it_to_a_ready_member_only() {
    let (e1, e2, e3) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
    let address = "198.51.100.2".parse().unwrap();
    let held = hold(e1, 7);
    let all = |h: [Health; 3]| [(e1, h[0]), (e2, h[1]), (e3, h[2])];
    let ready = all([Health::Ready; 3]);
    assert_eq!(designate(address, &ready, Some(&held), at(105)), Some(e1), "it stays put");
    let failing = all([Health::Failing, Health::Ready, Health::Failing]);
    assert_eq!(designate(address, &failing, Some(&held), at(105)), Some(e2));
    let blind = all([Health::Failing, Health::Unknown, Health::Failing]);
    assert_eq!(
        designate(address, &blind, Some(&held), at(105)),
        Some(e1),
        "no member passes: the control plane suspects itself and moves nothing"
    );
    let gone = all([Health::Disconnected, Health::Unknown, Health::Disconnected]);
    assert_eq!(designate(address, &gone, Some(&held), at(105)), Some(e2), "its session is down");
    let alone = all([Health::Disconnected; 3]);
    assert_eq!(designate(address, &alone, Some(&held), at(105)), Some(e1), "nothing better");
    let unreached = all([Health::Disconnected, Health::Failing, Health::Disconnected]);
    assert_eq!(
        designate(address, &unreached, None, at(105)),
        Some(e2),
        "a probe no member passes is suspect: the sessions decide"
    );
    let partly = all([Health::Failing, Health::Ready, Health::Unknown]);
    assert_eq!(designate(address, &partly, None, at(105)), Some(e2), "one passes: it decides");
    let spread: std::collections::BTreeSet<_> = (0..64_u8)
        .filter_map(|i| designate(IpAddr::from([198, 51, 100, i]), &ready, None, at(0)))
        .collect();
    assert_eq!(spread.len(), 3, "a group's addresses spread over its members");
    let left = [(e2, Health::Ready), (e3, Health::Ready)];
    assert_ne!(designate(address, &left, Some(&held), at(105)), Some(e1), "no longer a member");
}

#[test]
fn an_address_is_issued_to_another_member_only_once_its_holder_s_lease_is_over() {
    let (e1, e2) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let held = hold(e1, 7);
    assert_eq!(decide(e1, Some(e1), Some(&held), at(105)), Decision::Renew { epoch: 7 });
    assert_eq!(
        decide(e2, Some(e2), Some(&held), at(105)),
        Decision::Refuse(Refusal::Unavailable),
        "the next member waits out the holder's term"
    );
    assert_eq!(decide(e2, Some(e2), Some(&held), at(110)), Decision::Issue, "the term is over");
    let released = Hold { released_at: Some(at(103)), ..held };
    assert_eq!(decide(e2, Some(e2), Some(&released), at(104)), Decision::Issue, "or released");
    assert_eq!(decide(e1, Some(e1), Some(&released), at(104)), Decision::Issue, "a new epoch");
    assert_eq!(
        decide(e1, Some(e2), Some(&held), at(105)),
        Decision::Refuse(Refusal::NotAssigned),
        "a holder no longer designated is not renewed"
    );
    assert_eq!(
        decide(e2, Some(e1), Some(&held), at(105)),
        Decision::Refuse(Refusal::Superseded { by: e1 })
    );
    assert_eq!(decide(e1, Some(e1), None, at(0)), Decision::Issue);
}

#[test]
fn a_stable_endpoint_is_unreached_without_a_holder_a_route_to_it_or_a_ready_member() {
    let (e1, e2) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let address = "198.51.100.2".parse().unwrap();
    let state = |node, ready| MemberState {
        group: Uuid::nil(),
        node,
        connected: true,
        ready,
        probed_at: None,
        reason: None,
    };
    let members = [state(e1, Some(true)), state(e2, Some(false))];
    let status =
        |route| AddressStatus { hold: hold(e1, 7), answering: None, probed_at: None, route };
    let held = [status(None)];
    assert_eq!(unreached((&members, &held), address, at(105)), None);
    assert!(unreached((&members, &held), address, at(110)).unwrap().contains("holds"), "over");
    let blind = [state(e1, None), state(e2, Some(false))];
    assert!(unreached((&blind, &held), address, at(105)).unwrap().contains("probe"));

    let routed = |to| Some(EdgeRoute { router: "bgp".into(), routed: to, failing: None });
    let elsewhere = [status(routed(Some(e2)))];
    let why = unreached((&members, &elsewhere), address, at(105)).unwrap();
    assert!(why.contains("router"), "routed to the last holder: {why}");
    assert_eq!(unreached((&members, &[status(routed(Some(e1)))]), address, at(105)), None);
}
