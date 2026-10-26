// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::panic, reason = "a failing test should fail loudly")]

use super::*;
use std::time::Duration;

fn told(addresses: &[&str]) -> agent::Edges {
    let hold = addresses
        .iter()
        .map(|a| agent::EdgeAddress {
            address: (*a).into(),
            interface: "eth1".into(),
            ..Default::default()
        })
        .collect();
    agent::Edges {
        groups: vec![agent::EdgeForwarding { hold, ..Default::default() }],
        ..Default::default()
    }
}

fn granted(asked: &agent::AgentMessage, epoch: u64) -> Event {
    let Some(Up::EdgeLeaseRequest(r)) = &asked.message else { panic!("{asked:?}") };
    Event::EdgeLeaseGrant(agent::EdgeLeaseGrant {
        address: r.address.clone(),
        nonce: r.nonce.clone(),
        epoch,
        term: korpis_wire::duration(Duration::from_secs(10)).into(),
        rho_ppm: 1_000,
        ..Default::default()
    })
}

#[test]
fn an_address_is_held_only_under_a_granted_lease_and_released_once_no_longer_held() {
    let mut leases = EdgeLeases::default();
    let now = Instant::now();
    let table = told(&["198.51.100.2"]);
    let asked = leases.due(&table, now);
    assert_eq!(asked.len(), 1, "asked for at once");
    assert!(leases.holding(&table, now).is_empty(), "nothing is held before the grant");
    assert!(leases.answer(&granted(&asked[0], 7), now));
    let holding = leases.holding(&table, now);
    assert_eq!(holding.get("198.51.100.2"), Some(&("eth1".to_owned(), 7)));
    assert!(leases.kept(holding).is_empty(), "nothing given up");
    assert!(leases.due(&table, now).is_empty(), "renewed only a third of the way through");
    let later = now + Duration::from_secs(10);
    assert!(leases.holding(&table, later).is_empty(), "never past its term on this clock");

    let gone = told(&[]);
    let _ = leases.due(&gone, now);
    let released = leases.kept(leases.holding(&gone, now));
    let Some(Up::EdgeLeaseReleased(r)) = &released[0].message else { panic!("{released:?}") };
    assert_eq!((r.address.as_str(), r.epoch), ("198.51.100.2", 7));
}
