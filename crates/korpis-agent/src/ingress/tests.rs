// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::network::Pools;
use korpis_proto::korpis::agent::v1 as agent;

fn endpoint(name: &str, protocol: pb::EndpointProtocol, exposure: pb::Exposure) -> pb::Endpoint {
    pb::Endpoint {
        name: name.into(),
        protocol: protocol.into(),
        container_port: 8080,
        exposure: exposure.into(),
        domains: if exposure == pb::Exposure::EXPOSURE_INGRESS {
            vec![format!("{name}.example.com")]
        } else {
            Vec::new()
        },
        ..Default::default()
    }
}

#[test]
fn each_ingress_endpoint_of_a_workload_with_an_address_is_a_route() {
    let dir = tempfile::tempdir().unwrap();
    let mut addresses = Addresses::open(&dir.path().join("addresses")).unwrap();
    let pools = Pools { ipv4: "10.47.0.0/16".parse().unwrap(), ipv6: None };
    addresses.assign("w1", pools).unwrap();
    let http = pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP;
    let tcp = pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP;
    let mut tls = endpoint("tls", tcp, pb::Exposure::EXPOSURE_INGRESS);
    tls.tls =
        pb::TlsSpec { source: pb::TlsSource::TLS_SOURCE_PASSTHROUGH.into(), ..Default::default() }
            .into();
    let w1 = agent::Assignment {
        endpoints: vec![
            endpoint("web", http, pb::Exposure::EXPOSURE_INGRESS),
            endpoint("game", tcp, pb::Exposure::EXPOSURE_NODE_BOUND),
            tls,
        ],
        ..Default::default()
    };
    let unaddressed = agent::Assignment {
        endpoints: vec![endpoint("other", http, pb::Exposure::EXPOSURE_INGRESS)],
        ..Default::default()
    };
    let mut held = HeldSet::default();
    held.assignments.insert("w1".into(), w1);
    held.assignments.insert("w2".into(), unaddressed);
    let c = config(&held, &addresses, Vec::new());
    let routes: Vec<_> = c
        .routes
        .iter()
        .map(|r| (r.names[0].as_str(), r.backend.as_str(), r.mode.as_known()))
        .collect();
    assert_eq!(
        routes,
        [
            ("web.example.com", "10.47.0.1:8080", Some(ingress::RouteMode::ROUTE_MODE_HTTP)),
            ("tls.example.com", "10.47.0.1:8080", Some(ingress::RouteMode::ROUTE_MODE_PASSTHROUGH)),
        ],
        "w2 has no address yet"
    );
}
