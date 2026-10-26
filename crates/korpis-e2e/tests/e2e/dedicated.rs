// SPDX-License-Identifier: AGPL-3.0-or-later

//! A dedicated address on a real node (§3.7 of docs/07-networking.md, trace 77 of
//! docs/23-walkthroughs.md). The node's data plane is in a network namespace of the test's own,
//! linked to another standing in for its network, as the mesh's test has it. A pool of the
//! node's organization has addresses on that link; a game server with a `dedicated_ip` endpoint
//! is given its lowest, which the node answers for on the link and translates to the server,
//! its declared port reached there and no other, and what the server sends out leaves from it.
//! Retiring the server takes the address off the link and gives it back to the pool, which can
//! then be deleted. Needs root and containerd; skipped without them.

use crate::dependencies::running;
use crate::mesh::{Netns, fetched, net_of, node_in, served};
use crate::node::live;
use crate::recipes::push_server;
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use std::net::Ipv4Addr;
use std::process::Command;

/// The client's namespace and the node's, linked: the client at `198.18.3.254/24`, the node at
/// `198.18.3.1/24`, on the node's interface named second.
fn network() -> (Netns, Netns, String) {
    let id = std::process::id();
    let (client, node) =
        (Netns::new(format!("korpis-ded-{id}-c")), Netns::new(format!("korpis-ded-{id}-n")));
    let (vc, vn) = (format!("kded{}c", id % 100_000), format!("kded{}n", id % 100_000));
    let made = Command::new("ip")
        .args(["link", "add", &vc, "netns", client.name(), "type", "veth", "peer", "name", &vn])
        .args(["netns", node.name()])
        .status()
        .unwrap();
    assert!(made.success());
    for (ns, link, address) in [(&client, &vc, "198.18.3.254/24"), (&node, &vn, "198.18.3.1/24")] {
        assert!(ns.run(&["ip", "addr", "add", address, "dev", link]));
        assert!(ns.run(&["ip", "link", "set", link, "up"]));
    }
    // The node's namespace forwards, as a node does; this machine's setting is left be.
    assert!(node.run(&["sysctl", "-qw", "net.ipv4.ip_forward=1"]));
    (client, node, vn)
}

fn mutation(key: &str) -> pb::Mutation {
    pb::Mutation { idempotency_key: key.to_owned(), ..Default::default() }
}

fn pools(cp: &ControlPlane) -> pb::AddressPoolServiceClient<connectrpc::client::HttpClient> {
    pb::AddressPoolServiceClient::new(cp.http(), cp.config())
}

/// A pool of the root organization, on `interface` of `node`'s link.
async fn pool_on(cp: &ControlPlane, node: &str, interface: &str) -> pb::AddressPool {
    let pool = pb::AddressPool {
        organization: cp.root_org.clone(),
        cidr: "198.18.3.16/28".into(),
        scope: pb::AddressPoolScope::ADDRESS_POOL_SCOPE_NODE.into(),
        node: node.to_owned(),
        interface: interface.to_owned(),
        ..Default::default()
    };
    let r = pb::CreateAddressPoolRequest {
        mutation: mutation("pool").into(),
        pool: pool.into(),
        ..Default::default()
    };
    pools(cp).create_address_pool(r).await.unwrap().into_owned().pool.into_option().unwrap()
}

/// What `client` is told of the address it was reached from, listening on `198.18.3.254:9000`
/// while `ask` connects to it.
async fn source_seen(client: &Netns, ask: impl std::future::Future<Output = String>) -> String {
    let net = format!("--net={}", client.path().display());
    let script = "import socket\ns=socket.socket()\ns.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)\n\
                  s.bind(('198.18.3.254',9000))\ns.listen()\ns.settimeout(30)\nc,a=s.accept()\n\
                  c.sendall(b'seen')\nprint(a[0])";
    let listen = tokio::task::spawn_blocking(move || {
        Command::new("nsenter").arg(net).args(["python3", "-c", script]).output().unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let _ = ask.await;
    String::from_utf8_lossy(&listen.await.unwrap().stdout).trim().to_owned()
}

fn on_link(node: &Netns, interface: &str) -> String {
    let net = format!("--net={}", node.path().display());
    let out = Command::new("nsenter")
        .args([net.as_str(), "ip", "-4", "-o", "addr", "show", "dev", interface])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Retires `arena` and waits for its address to leave `interface` of `machine` and go back to
/// `pool`, which is then deleted, none of its addresses held.
async fn retired(
    cp: &ControlPlane,
    (arena, pool): (&str, &str),
    (machine, interface): (&Netns, &str),
) {
    cp.retire(arena).await;
    for _ in 0..120 {
        if !on_link(machine, interface).contains("198.18.3.17") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(!on_link(machine, interface).contains("198.18.3.17"), "taken off the link");
    // The address goes back to the pool once the node reports the server removed.
    let mut deleted = None;
    for attempt in 0..120 {
        let r = pb::DeleteAddressPoolRequest {
            mutation: mutation(&format!("unpool-{attempt}")).into(),
            id: pool.to_owned(),
            ..Default::default()
        };
        deleted = Some(pools(cp).delete_address_pool(r).await);
        if deleted.as_ref().is_some_and(Result::is_ok) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(deleted.is_some_and(|d| d.is_ok()), "none of its addresses is held now");
}

#[tokio::test]
async fn a_game_server_is_reached_at_an_address_of_its_own_and_sends_from_it() {
    if !live() {
        return;
    }
    let (client, machine, interface) = network();
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = node_in(&cp, "ded-1", &machine, Ipv4Addr::new(198, 18, 3, 1)).await;
    let agent = node.agent().await;
    let named = node.state.load().unwrap().node;
    let pool = pool_on(&cp, &named, &interface).await;

    let http = pb::EndpointSpec {
        name: "http".into(),
        exposure: pb::Exposure::EXPOSURE_DEDICATED_IP.into(),
        ..Default::default()
    };
    let body = pb::IntentBody { endpoints: vec![http], ..Default::default() };
    let arena = cp.declare_body(&project, "arena", &push_server().await, body).await;
    node.guard(&arena);
    running(&arena).await;
    let status = cp.status(&arena).await;
    let address = status.endpoints[0].address.as_option().expect("an address of its own");
    assert_eq!(
        (address.ips.as_slice(), address.port),
        (["198.18.3.17".to_owned()].as_slice(), 8080)
    );

    let from = format!("--net={}", client.path().display());
    let answered = served(&from, "198.18.3.17:8080").await;
    assert!(answered.contains("served"), "its declared port, at its own address: {answered:?}");
    assert!(
        fetched(&from, "198.18.3.17:8081", None).await.is_empty(),
        "no port it does not declare"
    );
    assert!(on_link(&machine, &interface).contains("198.18.3.17/32"), "answered on the link");
    let sent =
        source_seen(&client, fetched(&net_of(&node, &arena), "198.18.3.254:9000", None)).await;
    assert_eq!(sent, "198.18.3.17", "what it sends leaves from its own address");

    retired(&cp, (&arena, &pool.id), (&machine, &interface)).await;
    agent.stop().await;
}
