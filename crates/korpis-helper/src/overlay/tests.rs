// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn request(peers: Vec<helper::OverlayPeer>) -> helper::KeepOverlayRequest {
    helper::KeepOverlayRequest { port: 51820, peers, ..Default::default() }
}

fn peer(key: Vec<u8>, endpoint: &str, allowed: &[&str]) -> helper::OverlayPeer {
    helper::OverlayPeer {
        public_key: key,
        endpoint: endpoint.into(),
        allowed_ips: allowed.iter().map(|a| (*a).to_owned()).collect(),
        ..Default::default()
    }
}

#[test]
fn a_peer_is_taken_with_its_key_its_endpoint_if_it_has_one_and_its_pools_as_networks() {
    let r = request(vec![
        peer(vec![1; 32], "203.0.113.7:51820", &["10.48.4.0/22", "fd12::/64"]),
        peer(vec![2; 32], "", &["10.48.8.1/22"]),
    ]);
    let taken = peers(&r).unwrap();
    assert_eq!(taken[0].endpoint, Some("203.0.113.7:51820".parse().unwrap()));
    assert_eq!(taken[0].allowed.len(), 2);
    assert_eq!(taken[1].endpoint, None, "one behind NAT is reached once it has reached this node");
    assert_eq!(taken[1].allowed, ["10.48.8.0/22".parse::<IpNet>().unwrap()], "its network");
}

#[test]
fn a_peer_named_wrongly_is_refused() {
    for bad in [
        peer(vec![1; 31], "", &["10.48.4.0/22"]),
        peer(vec![1; 32], "203.0.113.7", &["10.48.4.0/22"]),
        peer(vec![1; 32], "", &["10.48.4.0"]),
    ] {
        assert!(matches!(peers(&request(vec![bad])), Err(HelperError::Refused(_))));
    }
}

#[test]
fn the_device_updates_its_peers_in_place_removes_the_stale_and_keeps_its_own_key() {
    let taken = peers(&request(vec![peer(vec![1; 32], "", &["10.48.4.0/22"])])).unwrap();
    let attrs = device(None, 51820, (&taken, &[[9; 32]]));
    assert!(!attrs.iter().any(|a| matches!(a, WireguardAttribute::PrivateKey(_))), "kept key");
    assert!(!attrs.iter().any(|a| matches!(a, WireguardAttribute::Flags(_))), "no replacement");
    assert!(attrs.contains(&WireguardAttribute::ListenPort(51820)));
    let Some(WireguardAttribute::Peers(p)) = attrs.last() else { panic!("{attrs:?}") };
    let flags = WireguardPeerAttribute::Flags(WireguardPeerFlags::ReplaceAllowedIps);
    assert!(p[0].0.contains(&flags));
    assert!(p[0].0.contains(&WireguardPeerAttribute::PersistentKeepalive(KEEPALIVE_SECONDS)));
    assert!(p[1].0.contains(&WireguardPeerAttribute::Flags(WireguardPeerFlags::RemoveMe)));
    let fresh = device(Some([7; 32]), 51820, (&taken, &[]));
    assert!(fresh.contains(&WireguardAttribute::PrivateKey([7; 32])), "a new link's key");
}
