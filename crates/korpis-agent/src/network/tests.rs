// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn pools(ipv4: &str, ipv6: Option<&str>) -> Pools {
    Pools { ipv4: ipv4.parse().unwrap(), ipv6: ipv6.map(|p| p.parse().unwrap()) }
}

#[test]
fn addresses_are_the_lowest_free_and_survive_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("addresses");
    let p = pools("10.47.0.0/16", Some("fd12:3456:789a::/64"));
    let mut a = Addresses::open(&path).unwrap();
    let first = a.assign("w1", p).unwrap();
    assert_eq!(first.ipv4, Ipv4Addr::new(10, 47, 0, 1));
    assert_eq!(first.ipv6, Some("fd12:3456:789a::1".parse().unwrap()));
    assert_eq!(a.assign("w2", p).unwrap().ipv4, Ipv4Addr::new(10, 47, 0, 2));
    assert_eq!(a.assign("w1", p).unwrap(), first, "a workload keeps what it holds");
    a.release("w1").unwrap();
    assert_eq!(a.assign("w3", p).unwrap().ipv4, Ipv4Addr::new(10, 47, 0, 1), "freed is reused");
    let reopened = Addresses::open(&path).unwrap();
    assert_eq!(reopened.workloads().cloned().collect::<Vec<_>>(), ["w2", "w3"]);
}

#[test]
fn a_changed_pool_renumbers_and_a_full_one_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = Addresses::open(&tmp.path().join("addresses")).unwrap();
    a.assign("w1", pools("10.47.0.0/16", None)).unwrap();
    let moved = a.assign("w1", pools("10.48.0.0/30", None)).unwrap();
    assert_eq!(moved.ipv4, Ipv4Addr::new(10, 48, 0, 1));
    a.assign("w2", pools("10.48.0.0/30", None)).unwrap();
    assert!(a.assign("w3", pools("10.48.0.0/30", None)).is_err(), "a /30 has two host addresses");
}

#[test]
fn the_api_is_where_the_node_enrolled() {
    assert_eq!(api_of("https://cp.example:7443"), Some(("cp.example".into(), 7443)));
    assert_eq!(api_of("https://cp.example"), Some(("cp.example".into(), 443)));
    assert_eq!(api_of("https://[2001:db8::1]:7443"), Some(("2001:db8::1".into(), 7443)));
    assert_eq!(api_of("not a url"), None);
}

#[test]
fn a_node_s_unique_local_prefix_is_its_own_and_stable() {
    let one = unique_local("node-a");
    assert_eq!(one.prefix_len(), 64);
    assert_eq!(one.network().octets()[0], 0xfd);
    assert_eq!(one, unique_local("node-a"));
    assert_ne!(one, unique_local("node-b"));
}

#[test]
fn a_node_takes_the_pool_its_operator_names_else_the_one_it_enrolled_with_else_the_default() {
    let named: Ipv4Net = "192.168.200.0/24".parse().unwrap();
    assert_eq!(ipv4_pool(Some(named), "10.48.4.0/22"), named);
    assert_eq!(ipv4_pool(None, "10.48.4.0/22"), "10.48.4.0/22".parse::<Ipv4Net>().unwrap());
    let old = ipv4_pool(None, "");
    assert_eq!(old, DEFAULT_IPV4.parse::<Ipv4Net>().unwrap(), "enrolled before pools were given");
}
