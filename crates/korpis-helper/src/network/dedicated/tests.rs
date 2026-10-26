// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;

fn ip(a: &str) -> Ipv4Addr {
    a.parse().unwrap()
}

#[test]
fn a_dedicated_address_is_translated_ahead_of_every_other_translation() {
    let rows = serde_json::to_string(&base()).unwrap();
    assert!(rows.contains("\"prio\":-110"), "{rows}");
    assert!(rows.contains("dedicated_prerouting") && rows.contains("dedicated_output"), "{rows}");
    let out = serde_json::to_string(&outward("postrouting")).unwrap();
    assert!(out.contains("snat") && out.contains("@dedicated_out"), "{out}");
    assert!(out.contains(crate::overlay::INTERFACE), "what enters the mesh keeps its source");
}

#[test]
fn a_workload_holding_an_address_points_both_maps_at_it_and_takes_one_over() {
    let link = "kwabc";
    let listed = Listed {
        inward: vec![(ip("203.0.113.5"), "ded_kwold".into()), (ip("203.0.113.9"), chain_of(link))],
        outward: vec![link.into()],
        added: Vec::new(),
    };
    let held = Dedicated { address: ip("203.0.113.5"), interface: Some("eth0".into()) };
    let rows =
        serde_json::to_string(&hold((link, ip("10.47.0.2")), Some(&held), Some("eth0"), &listed))
            .unwrap();
    assert!(rows.contains("\"203.0.113.9\""), "the address it held before is deleted: {rows}");
    assert!(rows.contains("\"on eth0\""), "the interface it was added to is said: {rows}");
    assert!(rows.contains("\"addr\":\"10.47.0.2\""), "{rows}");
    assert_eq!(rows.matches("\"203.0.113.5\"").count(), 3, "taken over, then added to both maps");
    let none = serde_json::to_string(&hold((link, ip("10.47.0.2")), None, None, &listed)).unwrap();
    assert!(!none.contains("dnat") && none.contains("\"delete\""), "{none}");
}

#[test]
fn only_an_address_the_helper_added_is_read_back_as_added() {
    let items: Vec<Value> = serde_json::from_value(json!([
        {"map": {"name": INWARD, "elem": [["203.0.113.5", {"goto": {"target": "ded_kwabc"}}]]}},
        {"map": {"name": OUTWARD, "elem": [["kwabc", "203.0.113.5"]]}},
        {"rule": {"chain": "ded_kwabc", "comment": "on eth0", "expr": []}},
        {"rule": {"chain": "ded_kwdef", "expr": []}},
        {"rule": {"chain": "in_kwabc", "comment": "on eth1", "expr": []}},
    ]))
    .unwrap();
    let listed = listed(&items);
    assert_eq!(listed.inward, [(ip("203.0.113.5"), "ded_kwabc".to_owned())]);
    assert_eq!(listed.outward, ["kwabc"]);
    assert_eq!(listed.added, [("ded_kwabc".to_owned(), "eth0".to_owned())]);
    assert_eq!(holding("kwabc", &listed), Some((ip("203.0.113.5"), Some("eth0".into()))));
    assert_eq!(holding("kwdef", &listed), None);
}
