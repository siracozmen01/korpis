// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

/// A self-signed certificate for `name` and its key, in PEM.
fn self_signed(name: &str) -> (String, String) {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec![name.to_owned()]).unwrap().self_signed(&key);
    (cert.unwrap().pem(), key.serialize_pem())
}

fn route(names: &[&str], backend: &str, mode: pb::RouteMode, cert: &str) -> pb::Route {
    pb::Route {
        names: names.iter().map(|n| (*n).to_owned()).collect(),
        backend: backend.into(),
        mode: mode.into(),
        certificate: cert.into(),
        workload: "wl".into(),
        endpoint: "web".into(),
        ..Default::default()
    }
}

fn config(routes: Vec<pb::Route>, certificates: Vec<pb::Certificate>) -> pb::IngressConfig {
    pb::IngressConfig { routes, certificates, ..Default::default() }
}

fn certificate(id: &str, (chain, key): (String, String)) -> pb::Certificate {
    pb::Certificate { id: id.into(), chain, key, ..Default::default() }
}

#[test]
fn a_configuration_is_served_whole_with_its_routes_by_name_in_any_case() {
    let cert = certificate("crt_1", self_signed("shop.example.com"));
    let http = pb::RouteMode::ROUTE_MODE_HTTP;
    let shop = route(&["Shop.example.com", "www.example.com"], "10.0.0.5:8080", http, "crt_1");
    let pass = pb::RouteMode::ROUTE_MODE_PASSTHROUGH;
    let tls = route(&["tls.example.com"], "[fd00::5]:8443", pass, "");
    let mut c = config(vec![shop, tls], vec![cert]);
    c.challenges = vec![pb::Challenge {
        token: "tok".into(),
        key_authorization: "tok.thumb".into(),
        ..Default::default()
    }];
    c.limits = pb::Limits { max_headers: 50, max_header_bytes: 1024, ..Default::default() }.into();
    let t = Table::from_pb(&c).unwrap();
    let r = t.route("SHOP.example.com:443").unwrap();
    assert_eq!((r.mode, r.backend.port()), (Mode::Http, 8080));
    assert!(r.certificate.is_some());
    assert!(Arc::ptr_eq(r, t.route("www.example.com.").unwrap()), "one route, two names");
    assert_eq!(t.route("tls.example.com").unwrap().mode, Mode::Passthrough);
    assert!(t.route("example.com").is_none());
    assert_eq!(t.challenge("tok"), Some("tok.thumb"));
    assert_eq!((t.limits.max_headers, t.limits.max_header_bytes), (50, 8 << 10), "8 KiB floor");
    assert_eq!(t.limits.max_connections, Limits::default().max_connections);
}

#[test]
fn a_configuration_with_anything_wrong_is_refused_whole() {
    let http = pb::RouteMode::ROUTE_MODE_HTTP;
    let ok = || route(&["a.example.com"], "10.0.0.5:80", http, "");
    let refused = |c: pb::IngressConfig| Table::from_pb(&c).unwrap_err().0;
    assert!(
        refused(config(vec![route(&["a.example.com"], "10.0.0.5", http, "")], vec![]))
            .contains("ip:port")
    );
    let none = pb::RouteMode::ROUTE_MODE_UNSPECIFIED;
    assert!(
        refused(config(vec![route(&["a.example.com"], "10.0.0.5:80", none, "")], vec![]))
            .contains("mode")
    );
    assert!(
        refused(config(vec![route(&["a.example.com"], "10.0.0.5:80", http, "crt_x")], vec![]))
            .contains("certificate")
    );
    assert!(refused(config(vec![ok(), ok()], vec![])).contains("routed twice"));
    let (chain, _) = self_signed("a.example.com");
    let (_, other_key) = self_signed("b.example.com");
    let mismatched = certificate("crt_1", (chain, other_key));
    assert!(refused(config(vec![], vec![mismatched])).contains("crt_1"), "a key not its own");
}

#[test]
fn a_host_is_compared_without_its_port_final_dot_or_case() {
    assert_eq!(normal("Shop.Example.com:8443"), "shop.example.com");
    assert_eq!(normal("shop.example.com."), "shop.example.com");
    assert_eq!(normal("[2001:db8::1]:443"), "[2001:db8::1]");
    assert_eq!(normal("2001:db8::1"), "2001:db8::1", "an unbracketed literal has no port");
}
