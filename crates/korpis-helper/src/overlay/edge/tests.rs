// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;

fn asked(edges: helper::EdgeTable) -> helper::KeepOverlayRequest {
    helper::KeepOverlayRequest {
        mesh_address: "10.48.3.255".into(),
        edges: edges.into(),
        ..Default::default()
    }
}

fn game(via: &str, to: &str) -> helper::EdgeTranslation {
    helper::EdgeTranslation {
        address: "198.51.100.2".into(),
        port: 25_565,
        transport: helper::Transport::TRANSPORT_TCP.into(),
        via: via.into(),
        to_address: to.into(),
        to_port: 25_565,
        ..Default::default()
    }
}

#[test]
fn a_table_is_checked_and_a_member_s_translation_is_marked_into_its_tunnel() {
    let edges = helper::EdgeTable {
        tunnels: vec!["10.48.7.255".into()],
        translations: vec![game("10.48.7.255", "10.48.4.2"), game("", "")],
        holds: vec![helper::EdgeHold { address: "198.51.100.2".into(), ..Default::default() }],
        admits: vec![helper::EdgeAdmit {
            source: "10.48.7.255".into(),
            destination: "10.48.3.255".into(),
            port: 65_535,
            transport: helper::Transport::TRANSPORT_UDP.into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let t = table(&asked(edges)).unwrap();
    assert_eq!(t.holds[0].interface, "lo", "a routed address is put on the loopback");
    assert_eq!(t.translations[1].to, None, "running nowhere: refused");
    assert_eq!(link_name("10.48.7.255".parse().unwrap()), "kg0a3007ff");
    let links =
        BTreeMap::from([("10.48.7.255".parse().unwrap(), ("kg0a3007ff".to_owned(), 7_u32))]);
    let (setup, before, forward, input) = rules(&t, &links);
    let text = serde_json::to_string(&setup).unwrap();
    assert!(text.contains(r#"{"dnat":{"addr":"10.48.4.2","family":"ip","port":25565}}"#), "{text}");
    assert!(text.contains(&EDGE_MARK.to_string()), "marked into its tunnel");
    assert!(text.contains(r#"["kg0a3007ff",1262878727]"#), "the link's own mark: {text}");
    assert!(text.contains(r#"{"reject":{"type":"tcp reset"}}"#), "refused, never dropped");
    assert!(serde_json::to_string(&before).unwrap().contains(r#""right":"gre""#));
    let admitted = serde_json::to_string(&[forward, input].concat()).unwrap();
    assert!(admitted.contains(r#""comment":"edge""#) && admitted.contains(r#""drop":null"#));

    let lost =
        asked(helper::EdgeTable { tunnels: vec!["10.48.7.255".into()], ..Default::default() });
    let unmeshed = helper::KeepOverlayRequest { mesh_address: String::new(), ..lost };
    assert!(table(&unmeshed).is_err(), "a tunnel needs a mesh address to start from");
    let mut bad = game("", "10.48.4.2");
    bad.transport = helper::Transport::TRANSPORT_UNSPECIFIED.into();
    let refused = helper::EdgeTable { translations: vec![bad], ..Default::default() };
    assert!(table(&asked(refused)).is_err());
}
