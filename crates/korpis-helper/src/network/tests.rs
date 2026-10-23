// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn p(text: &str) -> Prefix {
    prefix("pool", text).unwrap()
}

#[test]
fn a_host_link_name_fits_an_interface_name_and_is_stable() {
    let name = host_link("0192f1a2-7c3e-7d00-8a1b-3c4d5e6f7a8b");
    assert_eq!(name.len(), 15);
    assert!(name.starts_with("kw"));
    assert_eq!(name, host_link("0192f1a2-7c3e-7d00-8a1b-3c4d5e6f7a8b"));
    assert_ne!(name, host_link("0192f1a2-7c3e-7d00-8a1b-3c4d5e6f7a8c"));
}

#[test]
fn a_host_link_s_hardware_address_is_its_own_locally_administered_unicast_one() {
    let w = "0192f1a2-7c3e-7d00-8a1b-3c4d5e6f7a8b";
    let mac = host_mac(w);
    assert_eq!(mac[0], 0x02, "locally administered, and not multicast");
    assert_eq!(mac, host_mac(w));
    assert_ne!(mac, host_mac("0192f1a2-7c3e-7d00-8a1b-3c4d5e6f7a8c"));
}

#[test]
fn an_address_must_be_a_host_address_of_its_pool() {
    let pool = p("10.47.0.0/16");
    assert!(within("10.47.0.2".parse().unwrap(), pool));
    assert!(within("10.47.255.254".parse().unwrap(), pool));
    assert!(!within("10.47.0.0".parse().unwrap(), pool));
    assert!(!within("10.47.255.255".parse().unwrap(), pool));
    assert!(!within("10.48.0.2".parse().unwrap(), pool));
    assert!(!within("fd00::2".parse().unwrap(), pool));
    let v6 = p("fd47:1:2:3::/64");
    assert!(within("fd47:1:2:3::5".parse().unwrap(), v6));
    assert!(!within("fd47:1:2:4::5".parse().unwrap(), v6));
}

#[test]
fn a_prefix_is_refused_unless_it_is_one() {
    for bad in ["10.47.0.0", "10.47.0.0/33", "10.47.0.0/0", "x/8", "fd00::/129"] {
        assert!(prefix("pool", bad).is_err(), "{bad}");
    }
}

#[test]
fn a_request_naming_an_address_outside_its_pool_is_refused() {
    let mut r = helper::EnsureNetworkRequest {
        workload: "w1".into(),
        ipv4: "10.47.0.2".into(),
        ipv4_pool: "10.47.0.0/16".into(),
        uid_base: 1 << 20,
        uid_count: 1 << 16,
        ..Default::default()
    };
    assert!(asked(&r).is_ok());
    r.uid_count = 0;
    assert!(matches!(asked(&r), Err(HelperError::Refused(_))), "no range for the namespaces");
    r.uid_count = 1 << 16;
    r.ipv4 = "192.168.1.2".into();
    assert!(matches!(asked(&r), Err(HelperError::Refused(_))));
    r.ipv4 = "10.47.0.2".into();
    r.workload = "../etc".into();
    assert!(matches!(asked(&r), Err(HelperError::Refused(_))));
}
