// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's network on a real node (§4.1 of docs/07-networking.md): the agent allocates its
//! address, the helper makes its namespace and routed link, the container joins that namespace
//! rather than making its own, the host reaches it at its address, its declared policy holds
//! there (§6), and retiring the workload releases all of it.

use crate::node::{Node, live};
use crate::recipes::push_answerer;
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::network_rule::Peer;
use std::os::unix::fs::MetadataExt as _;
use std::process::Command;
use std::time::Duration;

async fn converged(cp: &ControlPlane, w: &str) {
    for _ in 0..900 {
        let s = cp.status(w).await;
        if s.convergence.as_known() == Some(pb::Convergence::CONVERGENCE_CONVERGED) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let failed = "SELECT coalesce(string_agg(action || ': ' || error::text, '; '), '') \
                  FROM effect WHERE error IS NOT NULL";
    panic!("{w} never converged; failed: {}", cp.scalar::<String>(failed).await);
}

/// The address the agent allocated `w`, from its addresses file.
fn address_of(node: &Node, w: &str) -> Option<String> {
    let kept = std::fs::read_to_string(node.state.root().join("addresses")).unwrap_or_default();
    kept.lines().find_map(|l| l.strip_prefix(&format!("{w} ")).map(str::to_owned))
}

/// The process ids of `w`'s running task, as containerd lists them.
fn task_pid(w: &str) -> Option<u32> {
    let ctr = |args: &[&str]| Command::new("ctr").args(["-n", "korpis"]).args(args).output();
    let filter = format!("labels.\"korpis.workload\"=={w}");
    let ids = ctr(&["containers", "ls", "-q", &filter]).ok()?;
    let id = String::from_utf8_lossy(&ids.stdout).split_whitespace().next()?.to_owned();
    let tasks = ctr(&["tasks", "ls"]).ok()?;
    let listed = String::from_utf8_lossy(&tasks.stdout).into_owned();
    let line = listed.lines().find(|l| l.starts_with(&id))?.to_owned();
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Asks `server` for `localhost`, which the host's resolver answers itself, from inside the
/// network namespace at `net`, over `transport`: `answered` with an address, `failed` with an
/// error code, or `silent`.
fn ask(net: &std::path::Path, server: &str, transport: &str) -> String {
    let script = format!(
        "import socket,struct\n\
         q=b'\\x12\\x34\\x01\\x00\\x00\\x01\\x00\\x00\\x00\\x00\\x00\\x00\\x09localhost\\x00\\x00\\x01\\x00\\x01'\n\
         try:\n\
         \x20if '{transport}'=='udp':\n\
         \x20 s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.settimeout(2)\n\
         \x20 s.sendto(q,('{server}',53));a=s.recv(512)\n\
         \x20else:\n\
         \x20 s=socket.create_connection(('{server}',53),2);s.settimeout(2)\n\
         \x20 s.sendall(struct.pack('>H',len(q))+q);f=s.makefile('rb')\n\
         \x20 n=struct.unpack('>H',f.read(2))[0];a=f.read(n)\n\
         \x20ok=a[:2]==q[:2] and a[3]&15==0 and a[6:8]!=b'\\x00\\x00'\n\
         \x20print('answered' if ok else 'failed')\n\
         except Exception:\n\
         \x20print('silent')"
    );
    let out = Command::new("nsenter")
        .arg(format!("--net={}", net.display()))
        .args(["python3", "-c", &script])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// As [`ask`], on a thread of its own: the agent answering runs on this test's one thread.
async fn asked(net: &std::path::Path, server: &str, transport: &str) -> String {
    let (net, server, transport) = (net.to_owned(), server.to_owned(), transport.to_owned());
    tokio::task::spawn_blocking(move || ask(&net, &server, &transport)).await.unwrap()
}

#[tokio::test]
async fn a_workload_joins_its_own_routed_network_and_retiring_it_releases_it() {
    if !live() {
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "net-1").await;
    let agent = node.agent().await;
    let w = cp.declare(&project, "answerer", &push_answerer().await).await;
    node.guard(&w);
    converged(&cp, &w).await;

    let ip = address_of(&node, &w).expect("an address allocated");
    let first = node.pool.hosts().next().unwrap().to_string();
    assert_eq!(ip, first, "the lowest free address of the node's pool");
    let bound = node.netns.join(&w);
    let pid = task_pid(&w).expect("a running task");
    let joined = std::fs::metadata(format!("/proc/{pid}/ns/net")).unwrap().ino();
    let made = std::fs::metadata(bound.join("net")).unwrap().ino();
    assert_eq!(joined, made, "the network namespace made for it");
    let ping = Command::new("ping").args(["-c1", "-W2", &ip]).output().unwrap();
    assert!(ping.status.success(), "the host reaches the workload at its address");

    // Its resolver, named alone, answers it over UDP and TCP from inside its own namespace,
    // whatever the host's firewall; the metadata address answers nothing (§5.1 and §6 of
    // docs/07-networking.md).
    let conf = std::fs::read_to_string(format!("/proc/{pid}/root/etc/resolv.conf")).unwrap();
    assert_eq!(conf, "nameserver 169.254.1.53\n");
    let net = bound.join("net");
    assert_eq!(asked(&net, "169.254.1.53", "udp").await, "answered", "over UDP");
    assert_eq!(asked(&net, "169.254.1.53", "tcp").await, "answered", "over TCP");
    assert_eq!(asked(&net, "169.254.169.254", "udp").await, "silent", "the metadata address");

    cp.retire(&w).await;
    // As long as convergence is given: retiring is a plan, approved, applied, then the node's
    // next pass after its set no longer holds the workload.
    for _ in 0..900 {
        if address_of(&node, &w).is_none() && !bound.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(address_of(&node, &w).is_none(), "its address is freed");
    assert!(!bound.exists(), "its namespace is released");
    agent.stop().await;
}

/// What the server at `at` answers a request for its index, within two seconds.
async fn served(at: std::net::SocketAddr) -> Option<String> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let ask = async {
        let mut s = tokio::net::TcpStream::connect(at).await.ok()?;
        s.write_all(b"GET / HTTP/1.0\r\n\r\n").await.ok()?;
        let mut got = String::new();
        s.read_to_string(&mut got).await.ok()?;
        Some(got)
    };
    tokio::time::timeout(Duration::from_secs(2), ask).await.ok().flatten()
}

#[tokio::test]
async fn a_client_reaches_the_declared_port_through_the_node_and_no_other() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "net-2").await;
    let agent = node.agent().await;
    let w = cp.declare(&project, "server", &crate::recipes::push_server().await).await;
    node.guard(&w);
    converged(&cp, &w).await;

    let status = cp.status(&w).await;
    let http = status.endpoints.iter().find(|e| e.name == "http").expect("its endpoint");
    let address = http.address.as_option().expect("an address on the node");
    let host = crate::node::host_address();
    assert_eq!(address.ips, [host.to_string()], "the node's own address");
    let port = u16::try_from(address.port).unwrap();
    assert!(node.ports.contains(port), "{port} is of the node's range");
    let reached = served((host, port).into()).await;
    assert!(reached.is_some_and(|r| r.contains("served")), "the node's port reaches it");

    let ip = address_of(&node, &w).expect("its address on the node");
    let workload: std::net::Ipv4Addr = ip.parse().unwrap();
    assert!(served((workload, 8080).into()).await.is_some(), "its declared port is open");
    assert!(served((workload, 8081).into()).await.is_none(), "a port it does not declare is not");

    cp.retire(&w).await;
    for _ in 0..900 {
        if address_of(&node, &w).is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(served((host, port).into()).await.is_none(), "the port is closed with it");
    agent.stop().await;
}

/// A rule denying `cidr`.
fn deny(cidr: &str) -> pb::NetworkRule {
    pb::NetworkRule {
        action: pb::NetworkAction::NETWORK_ACTION_DENY.into(),
        peer: Some(Peer::Cidr(cidr.into())),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_declared_policy_reaches_the_node_and_holds_there() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "net-3").await;
    let agent = node.agent().await;
    let host = crate::node::host_address();
    let http = pb::EndpointSpec {
        name: "http".into(),
        policy: pb::NetworkPolicy {
            ingress: vec![deny(&format!("{host}/32"))],
            ..Default::default()
        }
        .into(),
        ..Default::default()
    };
    let body = pb::IntentBody {
        endpoints: vec![http],
        egress: pb::EgressPolicy { rules: vec![deny("198.51.100.0/24")], ..Default::default() }
            .into(),
        ..Default::default()
    };
    let repo = crate::recipes::push_server().await;
    let w = cp.declare_body(&project, "guarded", &repo, body).await;
    node.guard(&w);
    converged(&cp, &w).await;

    let status = cp.status(&w).await;
    let http = status.endpoints.iter().find(|e| e.name == "http").expect("its endpoint");
    let port = u16::try_from(http.address.as_option().expect("an address").port).unwrap();
    assert!(served((host, port).into()).await.is_none(), "the endpoint's own rule denies the node");
    let ip = address_of(&node, &w).expect("its address on the node");
    let link = korpis_helper::network::host_link(&w);
    let chain = std::process::Command::new("nft")
        .args(["list", "chain", "inet", "korpis", &format!("out_{link}")])
        .output()
        .unwrap();
    let chain = String::from_utf8_lossy(&chain.stdout).into_owned();
    assert!(chain.contains("198.51.100.0/24") && chain.contains("\"policy\""), "{ip}: {chain}");
    cp.retire(&w).await;
    agent.stop().await;
}
