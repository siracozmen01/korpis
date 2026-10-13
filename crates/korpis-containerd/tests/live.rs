// SPDX-License-Identifier: Apache-2.0

//! The generated client against the machine's containerd, when there is one: its version over
//! gRPC on the Unix socket, and the `korpis` namespace, created if missing. Nothing outside that
//! namespace is read or written.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "a failing test should fail loudly")]

use connectrpc::Protocol;
use connectrpc::client::{ClientConfig, Http2Connection};
use korpis_containerd::containerd::services::namespaces::v1 as namespaces;
use korpis_containerd::containerd::services::version::v1 as version;
use std::path::Path;

const SOCKET: &str = "/run/containerd/containerd.sock";

#[tokio::test]
async fn the_client_speaks_grpc_to_containerd_over_its_socket() {
    if !Path::new(SOCKET).exists() {
        return;
    }
    let authority: http::Uri = "http://localhost".parse().unwrap();
    let conn = Http2Connection::connect_unix(SOCKET, authority.clone()).await.unwrap().shared(64);
    let config = ClientConfig::new(authority).with_protocol(Protocol::Grpc);
    let v = version::VersionClient::new(conn.clone(), config.clone())
        .version(buffa_types::google::protobuf::Empty::default())
        .await
        .unwrap()
        .into_owned();
    assert!(v.version.starts_with('v') || v.version.starts_with('2'), "{}", v.version);
    let ns = namespaces::NamespacesClient::new(conn, config);
    let created = ns
        .create(namespaces::CreateNamespaceRequest {
            namespace: namespaces::Namespace { name: "korpis".into(), ..Default::default() }.into(),
            ..Default::default()
        })
        .await;
    if let Err(e) = created {
        assert_eq!(e.code, connectrpc::ErrorCode::AlreadyExists, "{e}");
    }
    let got = ns
        .get(namespaces::GetNamespaceRequest { name: "korpis".into(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(got.namespace.as_option().unwrap().name, "korpis");
}
