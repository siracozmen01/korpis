// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's identity socket against a real control plane (§9.5 of docs/08-identity.md):
//! the agent serves one per workload it runs, a request on it comes back with a credential
//! that authenticates as that workload, over HTTP/2 and plain HTTP/1.1 alike, and the socket
//! goes when the workload does. A workload holding a grant is relayed the API beside it, a
//! task's while its run's object is its only one, until the grant is revoked.

use crate::session::until;
use crate::support::{ControlPlane, facts, mutation};
use connectrpc::ErrorCode;
use connectrpc::client::{ClientConfig, Http2Connection};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_extension::CredentialSource as _;
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

/// The credential, asked for over HTTP/2 as a generated client asks, and asked again while the
/// socket says to, as a workload does until its node holds the lease.
async fn over_http2(socket: &Path) -> agent::Credential {
    let authority = http::Uri::from_static("http://localhost");
    let conn = Http2Connection::lazy_unix(socket, authority.clone()).shared(4);
    let client = agent::WorkloadIdentityServiceClient::new(conn, ClientConfig::new(authority));
    for _ in 0..100 {
        match client.get_credential(agent::GetCredentialRequest::default()).await {
            Ok(answer) => return answer.into_owned().credential.into_option().unwrap(),
            Err(e) if e.code == ErrorCode::Unavailable => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => panic!("{e:?}"),
        }
    }
    panic!("the socket never issued a credential");
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
    // The SDK an extension is written with reads the same socket.
    let sdk = korpis_extension::IdentitySocket::at(&socket).fetch().await.unwrap();
    assert!(sdk.secret.starts_with("korpiswl_") && sdk.control_plane == cp.url, "{sdk:?}");
    assert!(sdk.ca_pem.starts_with(b"-----BEGIN CERTIFICATE-----"));
    // It also says where to use it, and how to trust the control plane there.
    let at = format!("\"controlPlane\":\"{}\"", cp.url);
    assert!(plain.contains(&at) && plain.contains("\"caPem\":\""), "{plain}");

    cp.retire(&w).await;
    exists(&root.join(&w), false).await;
    drop(agent);
}

/// Issues `actions` on the root organization to workload `w`; the grant.
async fn granted(cp: &ControlPlane, w: &str, actions: &[&str]) -> String {
    let subject = pb::Subject {
        kind: Some(pb::__buffa::oneof::subject::Kind::Workload(w.to_owned())),
        ..Default::default()
    };
    let scope = pb::Scope {
        kind: Some(pb::__buffa::oneof::scope::Kind::Organization(cp.root_org.clone())),
        ..Default::default()
    };
    let issued = cp
        .client(pb::GrantServiceClient::new)
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("to-the-workload").into(),
            subject: subject.into(),
            actions: actions.iter().map(|a| (*a).to_owned()).collect(),
            scope: scope.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    issued.grant.as_option().unwrap().id.clone()
}

#[tokio::test]
async fn a_run_of_a_workload_holding_a_grant_is_relayed_the_api_until_it_is_revoked() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let runtime = InMemory::new();
    let (agent, root) = start(&cp, dir.path(), &runtime).await;
    let w = crate::runs::task(&cp, &project, "nightly", pb::RunPolicy::default()).await;
    let grant = granted(&cp, &w, &["backup.create", "repository.read"]).await;

    // Its run is its only runtime object, and the relay stays while it runs, pass after pass.
    let relay = root.join(&w).join("api.sock");
    exists(&relay, true).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(relay.exists(), "a run's object counts as the workload's being built");

    let revoke =
        pb::RevokeGrantRequest { mutation: mutation("r").into(), grant, ..Default::default() };
    cp.client(pb::GrantServiceClient::new).revoke_grant(revoke).await.unwrap();
    exists(&relay, false).await;
    assert!(root.join(&w).join("identity.sock").exists(), "its identity is still served");
    drop(agent);
}
