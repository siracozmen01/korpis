// SPDX-License-Identifier: AGPL-3.0-or-later

//! Against a fake identity socket, served as the agent serves one: it says to ask again a few
//! times, as before the node holds the lease, then answers with a credential, a different one
//! each time.

use super::*;
use connectrpc::{ConnectError, ConnectRpcService, RequestContext, Response, Router};
use hyper_util::rt::{TokioExecutor, TokioIo};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::WorkloadIdentityServiceExt as _;
use std::sync::atomic::{AtomicU32, Ordering};
use tower_service::Service as _;

const CONTROL_PLANE: &str = "https://cp.test:7443";

/// What the fake socket does.
struct Fake {
    /// How many askings are told to ask again first.
    unavailable: u32,
    /// Whether it refuses outright, as for a workload placed elsewhere.
    refuses: bool,
    /// How long each credential is good for.
    good_for: Duration,
    asked: AtomicU32,
    ca_pem: Vec<u8>,
}

#[expect(refining_impl_trait, reason = "async fn handlers for the generated trait")]
#[expect(
    clippy::unused_async_trait_impl,
    reason = "the generated trait's handlers are asynchronous"
)]
impl agent::WorkloadIdentityService for Fake {
    async fn get_credential(
        &self,
        _ctx: RequestContext,
        _request: connectrpc::ServiceRequest<'_, agent::GetCredentialRequest>,
    ) -> connectrpc::ServiceResult<agent::GetCredentialResponse> {
        let n = self.asked.fetch_add(1, Ordering::SeqCst);
        if self.refuses {
            return Err(ConnectError::permission_denied("placed elsewhere"));
        }
        if n < self.unavailable {
            return Err(ConnectError::unavailable("no lease yet"));
        }
        let expires = SystemTime::now() + self.good_for;
        let seconds = expires.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        let credential = agent::Credential {
            secret: format!("korpiswl_{n}"),
            expires_at: buffa_types::google::protobuf::Timestamp {
                seconds: i64::try_from(seconds).unwrap(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        Response::ok(agent::GetCredentialResponse {
            credential: credential.into(),
            control_plane: CONTROL_PLANE.into(),
            ca_pem: self.ca_pem.clone(),
            ..Default::default()
        })
    }
}

/// `fake` served at `identity.sock` in `dir`; the socket's path.
fn served(dir: &Path, fake: Fake) -> PathBuf {
    let path = dir.join("identity.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let service = ConnectRpcService::new(Arc::new(fake).register(Router::new()));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(one(service.clone(), stream));
        }
    });
    path
}

/// One connection to the fake socket, served as the agent serves it.
async fn one(service: ConnectRpcService, stream: tokio::net::UnixStream) {
    let hyper = hyper::service::service_fn(move |request| {
        let mut service = service.clone();
        async move { service.call(request).await }
    });
    let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    let _ = builder.serve_connection(TokioIo::new(stream), hyper).await;
}

fn fake(unavailable: u32, refuses: bool, good_for: Duration) -> Fake {
    let ca = rcgen::generate_simple_self_signed(vec!["cp.test".into()]).unwrap();
    let ca_pem = ca.cert.pem().into_bytes();
    Fake { unavailable, refuses, good_for, asked: AtomicU32::new(0), ca_pem }
}

/// The bearer `c`'s calls carry now.
fn bearer(c: &Connection) -> String {
    let config = c.config();
    let value = config.default_headers().get("authorization").unwrap();
    value.to_str().unwrap().to_owned()
}

#[tokio::test]
async fn the_credential_is_asked_for_until_given_and_bears_every_call_to_the_control_plane() {
    let dir = tempfile::tempdir().unwrap();
    let socket = served(dir.path(), fake(2, false, Duration::from_secs(900)));
    let c = connection(&socket).await.unwrap();
    assert_eq!(c.server(), CONTROL_PLANE, "the address the socket named");
    assert_eq!(bearer(&c), "Bearer korpiswl_2", "asked a third time, after two to ask again");
    // No relay beside the socket: the control plane is reached over the network, and a relay
    // the control plane does not serve itself may be reached as well.
    assert!(c.at("https://relay.test:7444").is_ok());
}

#[tokio::test]
async fn a_refusal_is_said_at_once_and_not_asked_again() {
    let dir = tempfile::tempdir().unwrap();
    let socket = served(dir.path(), fake(0, true, Duration::from_secs(900)));
    let started = Instant::now();
    let e = connection(&socket).await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2), "not retried");
    assert!(e.to_string().contains("placed elsewhere"), "{e}");
}

#[tokio::test]
async fn through_the_node_s_relay_only_the_control_plane_is_reached() {
    let dir = tempfile::tempdir().unwrap();
    let socket = served(dir.path(), fake(0, false, Duration::from_secs(900)));
    let _relay = tokio::net::UnixListener::bind(dir.path().join("api.sock")).unwrap();
    let c = connection(&socket).await.unwrap();
    assert!(c.at(&format!("{CONTROL_PLANE}/")).is_ok(), "the control plane's own relay");
    let e = c.at("https://relay.test:7444").unwrap_err();
    assert!(e.to_string().contains("reaches the control plane alone"), "{e}");
}

#[tokio::test]
async fn a_credential_is_renewed_before_it_expires() {
    let dir = tempfile::tempdir().unwrap();
    // Good for barely more than the renewal margin, so it is renewed within a second or two.
    let socket = served(dir.path(), fake(0, false, RENEW_BEFORE + Duration::from_secs(1)));
    let c = connection(&socket).await.unwrap();
    assert_eq!(bearer(&c), "Bearer korpiswl_0");
    let renewed = async {
        while bearer(&c) == "Bearer korpiswl_0" {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), renewed).await.unwrap();
    assert!(bearer(&c).starts_with("Bearer korpiswl_"));
}

#[test]
fn a_named_socket_is_used_and_the_mounted_one_only_when_no_server_is_named() {
    let named = PathBuf::from("/elsewhere/identity.sock");
    assert_eq!(socket(Some(named.clone()), true), Some(named.clone()));
    assert_eq!(socket(Some(named.clone()), false), Some(named));
    assert_eq!(socket(None, true), None, "a server named is the operator's choice");
    if !Path::new(SOCKET).exists() {
        assert_eq!(socket(None, false), None, "not inside a workload");
    }
}
