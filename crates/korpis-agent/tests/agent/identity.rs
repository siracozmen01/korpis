// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's identity socket against a real control plane (§9.5 of docs/08-identity.md):
//! the agent serves one per workload it runs, a request on it comes back with a credential
//! that authenticates as that workload, over HTTP/2 and plain HTTP/1.1 alike, and the socket
//! goes when the workload does.

use crate::session::until;
use crate::support::{ControlPlane, facts};
use connectrpc::ErrorCode;
use connectrpc::client::{ClientConfig, Http2Connection};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Waits for `path` to exist, or not.
async fn exists(path: &Path, wanted: bool) {
    for _ in 0..200 {
        if path.exists() == wanted {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{} never {}", path.display(), if wanted { "appeared" } else { "went" });
}

/// The credential, asked for over HTTP/2 as a generated client asks.
async fn over_http2(socket: &Path) -> agent::Credential {
    let authority = http::Uri::from_static("http://localhost");
    let conn = Http2Connection::lazy_unix(socket, authority.clone()).shared(4);
    let client = agent::WorkloadIdentityServiceClient::new(conn, ClientConfig::new(authority));
    let answer = client.get_credential(agent::GetCredentialRequest::default()).await.unwrap();
    answer.into_owned().credential.into_option().unwrap()
}

/// The raw response to a JSON request over HTTP/1.1, as `curl --unix-socket` sends it.
async fn over_http1(socket: &Path) -> String {
    let mut s = tokio::net::UnixStream::connect(socket).await.unwrap();
    let request = "POST /korpis.agent.v1.WorkloadIdentityService/GetCredential HTTP/1.1\r\n\
                   Host: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\
                   Connection: close\r\n\r\n{}";
    s.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    s.read_to_string(&mut response).await.unwrap();
    response
}

async fn start(cp: &ControlPlane, dir: &Path, runtime: &InMemory) -> (Agent, PathBuf) {
    let (token, ca) = cp.token("fra-9").await;
    let state = StateDir::new(dir.join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let root = dir.join("identity");
    let options = Options {
        period: Duration::from_millis(200),
        identity_root: Some(root.clone()),
        ..Options::default()
    };
    let host = InMemoryHost::new();
    let agent = Agent::start(&state, runtime.clone(), host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    (agent, root)
}

#[tokio::test]
async fn a_running_workload_gets_its_credential_from_its_socket_and_loses_it_on_retirement() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let runtime = InMemory::new();
    let (agent, root) = start(&cp, dir.path(), &runtime).await;
    let w = cp.declare(&project, "mc").await;
    let socket = root.join(&w).join("identity.sock");
    exists(&socket, true).await;
    let mode = std::fs::metadata(&root).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o750, "no one outside the agent's group reaches a socket on the host");

    let credential = Box::pin(over_http2(&socket)).await;
    assert!(credential.secret.starts_with("korpiswl_"));
    let subject = credential.subject.as_option().and_then(|s| s.kind.clone());
    assert!(matches!(subject, Some(pb::__buffa::oneof::subject::Kind::Workload(id)) if id == w));
    let read = pb::GetWorkloadRequest { id: w.clone(), ..Default::default() };
    let e = cp.workloads_as(&credential.secret).get_workload(read).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "authenticated as the workload, which holds no grant");
    let plain = over_http1(&socket).await;
    assert!(plain.starts_with("HTTP/1.1 200") && plain.contains("korpiswl_"), "{plain}");

    cp.retire(&w).await;
    exists(&root.join(&w), false).await;
    drop(agent);
}
