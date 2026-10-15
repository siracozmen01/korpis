// SPDX-License-Identifier: AGPL-3.0-or-later

//! A `passthrough` route: the proxy reads only the name the client asks for and splices the
//! connection to the workload, which terminates TLS itself with its own certificate.

use crate::support::{Pki, Proxy, get, https, route, server_tls, workload};
use http::StatusCode;
use korpis_proto::korpis::ingress::v1 as pb;

#[tokio::test]
async fn a_passthrough_workload_terminates_its_own_tls() {
    let pki = Pki::new();
    let own = server_tls(&pki.leaf(&["tls.example.test"]));
    let backend = workload("own-tls", Some(own)).await;
    let mode = pb::RouteMode::ROUTE_MODE_PASSTHROUGH;
    let config = pb::IngressConfig {
        routes: vec![route(&["tls.example.test"], backend, mode, "")],
        ..Default::default()
    };
    let proxy = Proxy::start(&config);
    let (status, body) =
        https(&pki, proxy.https, "tls.example.test", false, get("/", "tls.example.test")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("workload=own-tls"), "{body}");
    assert!(body.contains("forwarded=\n"), "unread: the proxy added nothing: {body}");
}
