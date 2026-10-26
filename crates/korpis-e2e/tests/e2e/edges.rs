// SPDX-License-Identifier: AGPL-3.0-or-later

//! An edge group on real nodes (§3.1 of docs/07-networking.md, trace 78 of
//! docs/23-walkthroughs.md). Two edges and a node, each with its data plane in a network
//! namespace of the test's own, share one link with a client, as machines share a network, so
//! nothing is made in this machine's own namespace. A server on the node, its endpoint `stable`,
//! is given an address and port of the group's pool on that link; the edge holding it under its
//! lease puts it on the link and forwards the client's connection through the mesh to the
//! server. When the holder loses power, its agent stopped and its link down, the control plane
//! hears nothing from it, and once its term has passed grants the other edge the address, which
//! announces it, and the client reaches the server through it (step 8 of the trace). Needs root,
//! containerd, and the kernel's WireGuard and GRE; skipped without them.
//!
//! The control plane runs in this machine's own namespace, which no route joins to the link, so
//! its probe reaches no edge, and it judges which edge holds the address by their sessions, as it
//! does whenever no member passes its probe; a group probed once a minute sends it few datagrams.

use crate::dependencies::running;
use crate::mesh::{Netns, cordon, fetched, node_in, served, until_available};
use crate::node::{Node, live};
use crate::recipes::push_server;
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use std::net::Ipv4Addr;
use std::process::Command;
use std::time::Duration;

/// The link's own namespace, holding the bridge every machine is wired to, and the machines: the
/// client at `198.18.4.254/24`, the edges at `.1` and `.2`, and the node at `.3`, each on its
/// interface `lan0`. `None` where this kernel makes no WireGuard or GRE link.
fn network() -> Option<(Netns, [Netns; 4])> {
    let id = std::process::id();
    let named = |n: &str| Netns::new(format!("korpis-edges-{id}-{n}"));
    let lan = named("lan");
    let capable = lan.run(&["ip", "link", "add", "kedge-wg", "type", "wireguard"])
        && lan.run(&["ip", "link", "del", "kedge-wg"])
        && lan.run(&["ip", "link", "add", "kedge-gre", "type", "gre", "remote", "198.18.9.9"])
        && lan.run(&["ip", "link", "del", "kedge-gre"]);
    if !capable {
        return None;
    }
    assert!(lan.run(&["ip", "link", "add", "br0", "type", "bridge"]));
    assert!(lan.run(&["ip", "link", "set", "br0", "up"]));
    let machines = ["c", "e1", "e2", "a"].map(named);
    for (i, (m, at)) in machines.iter().zip(["254", "1", "2", "3"]).enumerate() {
        let port = format!("kedg{}{i}", id % 10_000);
        let made = Command::new("ip")
            .args(["link", "add", &port, "netns", lan.name(), "type", "veth"])
            .args(["peer", "name", "lan0", "netns", m.name()])
            .status()
            .unwrap();
        assert!(made.success());
        assert!(lan.run(&["ip", "link", "set", &port, "master", "br0", "up"]));
        assert!(m.run(&["ip", "addr", "add", &format!("198.18.4.{at}/24"), "dev", "lan0"]));
        assert!(m.run(&["ip", "link", "set", "lan0", "up"]));
        // The test's own namespaces forward, as nodes do; this machine's setting is left be.
        assert!(m.run(&["sysctl", "-qw", "net.ipv4.ip_forward=1"]));
    }
    Some((lan, machines))
}

fn edges(cp: &ControlPlane) -> pb::EdgeGroupServiceClient<connectrpc::client::HttpClient> {
    pb::EdgeGroupServiceClient::new(cp.http(), cp.config())
}

fn mutation(key: &str) -> pb::Mutation {
    pb::Mutation { idempotency_key: key.to_owned(), ..Default::default() }
}

/// The group of `members`, probed once a minute, and its pool on `lan0` of the link.
async fn group_of(cp: &ControlPlane, members: &[&str]) -> pb::EdgeGroup {
    let group = pb::EdgeGroup {
        organization: cp.root_org.clone(),
        name: "edge".into(),
        members: members.iter().map(|m| (*m).to_owned()).collect(),
        probe_interval: korpis_wire::duration(Duration::from_secs(60)).into(),
        ..Default::default()
    };
    let r = pb::CreateEdgeGroupRequest {
        mutation: mutation("group").into(),
        group: group.into(),
        ..Default::default()
    };
    let made = edges(cp).create_edge_group(r).await.unwrap().into_owned().group.into_option();
    let made = made.unwrap();
    let pool = pb::AddressPool {
        organization: cp.root_org.clone(),
        cidr: "198.18.4.16/28".into(),
        scope: pb::AddressPoolScope::ADDRESS_POOL_SCOPE_EDGE.into(),
        group: made.id.clone(),
        interface: "lan0".into(),
        ..Default::default()
    };
    let r = pb::CreateAddressPoolRequest {
        mutation: mutation("pool").into(),
        pool: pool.into(),
        ..Default::default()
    };
    pb::AddressPoolServiceClient::new(cp.http(), cp.config()).create_address_pool(r).await.unwrap();
    made
}

/// Whether `address` is on `lan0` of `machine`.
fn holds(machine: &Netns, address: &str) -> bool {
    let out = Command::new("ip")
        .args(["-n", machine.name(), "-4", "-o", "addr", "show", "dev", "lan0"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&format!("{address}/32"))
}

/// Which of `edges` holds `address`, within a minute.
async fn holder(edges: [&Netns; 2], address: &str) -> usize {
    for _ in 0..120 {
        if let Some(i) = edges.iter().position(|m| holds(m, address)) {
            return i;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("no edge came to hold {address}");
}

#[tokio::test]
async fn a_client_reaches_a_server_at_its_stable_address_through_whichever_edge_holds_it() {
    if !live() {
        return;
    }
    let Some((_lan, [client, m1, m2, ma])) = network() else {
        eprintln!("this kernel makes no WireGuard or GRE link: not run");
        return;
    };
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let join = |name, m, at| node_in(&cp, name, m, Ipv4Addr::new(198, 18, 4, at));
    let (e1, e2, a): (Node, Node, Node) =
        (join("edge-1", &m1, 1).await, join("edge-2", &m2, 2).await, join("host-a", &ma, 3).await);
    let mut edge_agents = [Some(e1.agent().await), Some(e2.agent().await)];
    let agent_a = a.agent().await;
    let named = |n: &Node| n.state.load().unwrap().node;
    let (n1, n2) = (named(&e1), named(&e2));
    for n in [&e1, &e2, &a] {
        until_available(&cp, &named(n)).await;
    }
    cordon(&cp, &n1).await;
    cordon(&cp, &n2).await;
    group_of(&cp, &[&n1, &n2]).await;

    let http = pb::EndpointSpec {
        name: "http".into(),
        exposure: pb::Exposure::EXPOSURE_STABLE.into(),
        ..Default::default()
    };
    let body = pb::IntentBody { endpoints: vec![http], ..Default::default() };
    let arena = cp.declare_body(&project, "arena", &push_server().await, body).await;
    a.guard(&arena);
    running(&arena).await;
    let status = cp.status(&arena).await;
    let at = status.endpoints[0].address.as_option().expect("its group's address");
    let (address, port) = (at.ips[0].clone(), at.port);
    let to = format!("{address}:{port}");

    let from = format!("--net={}", client.path().display());
    let machines = [&m1, &m2];
    let first = holder(machines, &address).await;
    let answered = served(&from, &to).await;
    assert!(answered.contains("served"), "through the edge holding it: {answered:?}");
    assert!(fetched(&from, &format!("{address}:22"), None).await.is_empty(), "nothing else");

    // The holder loses power: what it held goes with it, and its session falls silent.
    if let Some(agent) = edge_agents[first].take() {
        agent.stop().await;
    }
    assert!(machines[first].run(&["ip", "link", "set", "lan0", "down"]));
    let other = 1 - first;
    // Three heartbeats unheard, then the rest of its term.
    for _ in 0..360 {
        if holds(machines[other], &address) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(holds(machines[other], &address), "the other edge is granted it");
    let answered = served(&from, &to).await;
    assert!(answered.contains("served"), "through the other edge now: {answered:?}");

    cp.retire(&arena).await;
    for agent in edge_agents.into_iter().flatten() {
        agent.stop().await;
    }
    agent_a.stop().await;
}
