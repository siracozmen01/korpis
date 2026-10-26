// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;
use std::time::UNIX_EPOCH;

const ORG: Uuid = Uuid::from_u128(1);
const NODE: Uuid = Uuid::from_u128(2);

fn pool(cidr: &str) -> AddressPool {
    AddressPool {
        id: Uuid::from_u128(9),
        organization: ORG,
        cidr: cidr.parse().unwrap(),
        scope: Scope::Node(NODE),
        routing: Routing::Static,
        interface: String::new(),
        quarantine: DEFAULT_QUARANTINE,
        reserved: Vec::new(),
        created_at: UNIX_EPOCH,
    }
}

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[test]
fn a_pool_says_what_can_hold_and_reaches_its_own_organization_s_nodes_alone() {
    assert_eq!(pool("203.0.113.0/28").validate(), Ok(()));
    let moving = AddressPool { routing: Routing::Bgp, ..pool("203.0.113.0/28") };
    assert_eq!(moving.validate(), Err(PoolError::Routing), "a node pool is static");
    let reserved =
        AddressPool { reserved: vec!["198.51.100.1".parse().unwrap()], ..pool("203.0.113.0/28") };
    assert!(matches!(reserved.validate(), Err(PoolError::Reserved(_))));
    let bad = AddressPool { interface: "eth0; rm".into(), ..pool("203.0.113.0/28") };
    assert!(matches!(bad.validate(), Err(PoolError::Interface(_))));
    let p = pool("203.0.113.0/28");
    assert!(p.reaches(NODE, ORG));
    assert!(!p.reaches(Uuid::from_u128(3), ORG), "another node");
    assert!(!p.reaches(NODE, Uuid::from_u128(4)), "a node of another organization");
    let cluster = AddressPool {
        scope: Scope::Cluster(Uuid::from_u128(5)),
        routing: Routing::ProviderApi,
        ..pool("203.0.113.0/28")
    };
    assert!(cluster.reaches(Uuid::from_u128(3), ORG) && !cluster.reaches(NODE, Uuid::from_u128(4)));
}

#[test]
fn the_lowest_address_neither_held_nor_resting_is_given() {
    let mut p = pool("203.0.113.0/29");
    p.reserved = vec!["203.0.113.1".parse().unwrap()];
    let first: IpAddr = "203.0.113.2".parse().unwrap();
    assert_eq!(lowest_free(&p, &BTreeMap::new(), at(0)), Some(first), "past the reserved one");
    let held = BTreeMap::from([(first, Taken::Held(Uuid::from_u128(7)))]);
    assert_eq!(lowest_free(&p, &held, at(0)), "203.0.113.3".parse().ok());
    let resting = BTreeMap::from([(first, Taken::Released(at(100)))]);
    assert_eq!(lowest_free(&p, &resting, at(200)), "203.0.113.3".parse().ok(), "still resting");
    let rested = at(100 + DEFAULT_QUARANTINE.as_secs());
    assert_eq!(lowest_free(&p, &resting, rested), Some(first), "its rest has ended");
    let all: BTreeMap<IpAddr, Taken> =
        p.addresses().map(|a| (a, Taken::Held(Uuid::from_u128(8)))).collect();
    assert_eq!(lowest_free(&p, &all, at(0)), None);
}
