// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ingress` through an edge group on real nodes (§3.1 of docs/07-networking.md, trace 78 of
//! docs/23-walkthroughs.md). Two edges serving the ingress, each its proxy on its own address on
//! the link it shares with a client, and a node serving none, each machine's data plane in a
//! network namespace of the test's own. A server's `ingress` endpoint, served with a certificate
//! its organization uploaded, is placed on the node; its name is reached by the client over TLS
//! at the group's ingress address, where the edge holding it translates port 443 to its own
//! proxy, which terminates TLS with the certificate it fetched and forwards through the mesh to
//! the server, whose node admits that edge's proxy. When the holder loses power, its agent
//! stopped and its link down, the other edge is granted the address and serves the name the same
//! way. Each time the name is reached over HTTP/3 as well, at UDP port 443 of the address, which
//! the holder translates to its proxy as it does TCP. Needs root, containerd, and the kernel's WireGuard and GRE; skipped without them.

use crate::dependencies::running;
use crate::edges::{comes_to_hold, group_of, holder, network};
use crate::ingress::attest;
use crate::mesh::{Netns, cordon, node_in, until_available};
use crate::node::{Node, live};
use crate::recipes::push_server;
use crate::support::{ControlPlane, mutation};
use base64::Engine as _;
use bytes::Buf as _;
use korpis_proto::korpis::v1 as pb;
use std::net::{Ipv4Addr, SocketAddr};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

const NAME: &str = "shop.example.com";

/// What `GET /` on `NAME` answers over TLS at port 443 of `address`, from inside the network
/// namespace `from` names, trusting `authority` alone; empty if nothing does within two seconds.
/// Python's strict checking, which asks for extensions the test's authority does not write, is
/// not asked for; the chain and the name are verified.
async fn over_tls(from: &str, address: &str, authority: &[u8]) -> String {
    let der = base64::engine::general_purpose::STANDARD.encode(authority);
    let script = format!(
        "import base64,socket,ssl\ngot=b''\ntry:\n \
         c=ssl.create_default_context(cadata=base64.b64decode('{der}'))\n \
         c.verify_flags&=~getattr(ssl,'VERIFY_X509_STRICT',0)\n \
         s=c.wrap_socket(socket.create_connection(('{address}',443),timeout=2),\
         server_hostname='{NAME}')\n \
         s.sendall(b'GET / HTTP/1.1\\r\\nhost: {NAME}\\r\\nconnection: close\\r\\n\\r\\n')\n \
         while True:\n  b=s.recv(4096)\n  if not b: break\n  got+=b\nexcept Exception as e: got+=repr(e).encode()\n\
         print(got.decode(errors='replace'), end='')"
    );
    let from = from.to_owned();
    let run = move || Command::new("nsenter").arg(from).args(["python3", "-c", &script]).output();
    let out = tokio::task::spawn_blocking(run).await.unwrap().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// That the server answers `NAME` at `address` within a minute, as `how` says.
async fn serves(from: &str, address: &str, authority: &[u8], how: &str) {
    let mut answered = String::new();
    for _ in 0..60 {
        answered = over_tls(from, address, authority).await;
        if answered.contains("served") {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("{how}: {answered:?}");
}

/// What `GET /` on `NAME` answers over HTTP/3 at UDP port 443 of `address`, from a socket in
/// `from`, trusting `authority` alone; why not, if it does not within five seconds.
async fn over_h3(from: &Netns, address: &str, authority: &[u8]) -> Result<String, String> {
    let why = |e: &dyn std::fmt::Display| e.to_string();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(authority.to_vec().into()).map_err(|e| why(&e))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| why(&e))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).map_err(|e| why(&e))?;
    let socket = from.udp();
    let runtime = Arc::new(quinn::TokioRuntime);
    let mut endpoint =
        quinn::Endpoint::new(quinn::EndpointConfig::default(), None, socket, runtime)
            .map_err(|e| why(&e))?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(crypto)));
    let at = SocketAddr::new(address.parse().map_err(|e| why(&e))?, 443);
    let fetched = async {
        let connection = endpoint.connect(at, NAME).map_err(|e| why(&e))?.await;
        let connection = h3_quinn::Connection::new(connection.map_err(|e| why(&e))?);
        let (mut driver, mut send) = h3::client::new(connection).await.map_err(|e| why(&e))?;
        tokio::spawn(async move { driver.wait_idle().await });
        let req = http::Request::get(format!("https://{NAME}/")).body(()).map_err(|e| why(&e))?;
        let mut stream = send.send_request(req).await.map_err(|e| why(&e))?;
        stream.finish().await.map_err(|e| why(&e))?;
        stream.recv_response().await.map_err(|e| why(&e))?;
        let mut body = Vec::new();
        while let Some(mut chunk) = stream.recv_data().await.map_err(|e| why(&e))? {
            body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    };
    tokio::time::timeout(Duration::from_secs(5), fetched).await.map_err(|e| why(&e))?
}

/// That the server answers `NAME` over HTTP/3 at `address` within a minute, as `how` says.
async fn serves_h3(from: &Netns, address: &str, authority: &[u8], how: &str) {
    let mut answered = Err(String::new());
    for _ in 0..30 {
        answered = over_h3(from, address, authority).await;
        if answered.as_ref().is_ok_and(|a| a.contains("served")) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("{how}, over HTTP/3: {answered:?}");
}

/// Uploads a certificate for `NAME`, issued by an authority of the test's own, whose name is its
/// own, so that OpenSSL takes the certificate for one it issued and not for one signed by itself;
/// the certificate's identifier, and the authority, DER-encoded.
async fn uploaded(cp: &ControlPlane) -> (String, Vec<u8>) {
    let named = |cn: &str| {
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, cn);
        dn
    };
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca.distinguished_name = named("korpis edge test authority");
    let authority = ca.self_signed(&ca_key).unwrap().der().to_vec();
    let issuer = rcgen::Issuer::new(ca, ca_key);
    let key = rcgen::KeyPair::generate().unwrap();
    let mut leaf = rcgen::CertificateParams::new(vec![NAME.to_owned()]).unwrap();
    leaf.distinguished_name = named(NAME);
    let chain = leaf.signed_by(&key, &issuer).unwrap().pem();
    let req = pb::CreateCertificateRequest {
        mutation: mutation("certificate").into(),
        organization: cp.root_org.clone(),
        chain,
        key: key.serialize_pem(),
        ..Default::default()
    };
    let client = pb::CertificateServiceClient::new(cp.http(), cp.config());
    let made = client.create_certificate(req).await.unwrap().into_owned();
    (made.certificate.into_option().unwrap().id, authority)
}

/// Waits until both edges are held as serving the ingress, which a group serving names needs of
/// two of its members.
async fn both_serve(cp: &ControlPlane) {
    for _ in 0..100 {
        if cp.scalar::<bool>("SELECT count(*) = 2 FROM node WHERE ingress").await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the edges' ingress was not reported within ten seconds");
}

/// The server's `web` endpoint: HTTP on 8080, routing `NAME`, served with `certificate`.
fn web(certificate: &str) -> pb::EndpointSpec {
    pb::EndpointSpec {
        name: "web".into(),
        protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP.into(),
        container_port: 8080,
        exposure: pb::Exposure::EXPOSURE_INGRESS.into(),
        domains: vec![NAME.into()],
        tls: pb::TlsSpec {
            source: pb::TlsSource::TLS_SOURCE_PROVIDED.into(),
            secret: certificate.into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_client_reaches_a_name_at_its_group_s_ingress_address_through_whichever_edge_holds_it() {
    if !live() {
        return;
    }
    let Some((_lan, [client, m1, m2, ma])) = network("ei") else {
        eprintln!("this kernel makes no WireGuard or GRE link: not run");
        return;
    };
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let join = |name, m, at| node_in(&cp, name, m, Ipv4Addr::new(198, 18, 4, at));
    let (e1, e2, a): (Node, Node, Node) =
        (join("edge-1", &m1, 1).await, join("edge-2", &m2, 2).await, join("host-a", &ma, 3).await);
    let mut edge_agents = [Some(e1.serving_ingress().await), Some(e2.serving_ingress().await)];
    let agent_a = a.agent().await;
    let named = |n: &Node| n.state.load().unwrap().node;
    let (n1, n2) = (named(&e1), named(&e2));
    for n in [&e1, &e2, &a] {
        until_available(&cp, &named(n)).await;
    }
    cordon(&cp, &n1).await;
    cordon(&cp, &n2).await;
    group_of(&cp, &[&n1, &n2]).await;
    both_serve(&cp).await;
    attest(&cp, "example.com").await;
    let (certificate, authority) = uploaded(&cp).await;

    // The recipe's own `http` endpoint is reached through the ingress alone.
    let unexposed = pb::EndpointSpec {
        name: "http".into(),
        exposure: pb::Exposure::EXPOSURE_NONE.into(),
        ..Default::default()
    };
    let endpoints = vec![unexposed, web(&certificate)];
    let body = pb::IntentBody { endpoints, ..Default::default() };
    let shop = cp.declare_body(&project, "shop", &push_server().await, body).await;
    a.guard(&shop);
    running(&shop).await;
    let status = cp.status(&shop).await;
    let web = status.endpoints.iter().find(|e| e.name == "web").expect("its web endpoint");
    let at = web.address.as_option().expect("its group's ingress address");
    let address = at.ips.iter().find(|a| !a.contains(':')).expect("its IPv4 address").clone();
    assert_eq!(at.port, 443);

    let from = format!("--net={}", client.path().display());
    let machines = [&m1, &m2];
    let first = holder(machines, &address).await;
    serves(&from, &address, &authority, "through the edge holding the ingress address").await;
    serves_h3(&client, &address, &authority, "through the edge holding it").await;

    // The holder loses power: what it held goes with it, and its session falls silent.
    if let Some(agent) = edge_agents[first].take() {
        agent.stop().await;
    }
    assert!(machines[first].run(&["ip", "link", "set", "lan0", "down"]));
    assert!(comes_to_hold(machines[1 - first], &address).await, "the other edge is granted it");
    serves(&from, &address, &authority, "through the other edge now").await;
    serves_h3(&client, &address, &authority, "through the other edge now").await;

    cp.retire(&shop).await;
    for agent in edge_agents.into_iter().flatten() {
        agent.stop().await;
    }
    agent_a.stop().await;
}
