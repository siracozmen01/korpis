// SPDX-License-Identifier: AGPL-3.0-or-later

//! The driver process's protocol over its socket, against the machine's containerd when there is
//! one and the test runs as root: Describe, Prepare, a container's life with the events it makes,
//! and a peer the kernel says is not allowed, refused before it is heard.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

use connectrpc::Protocol;
use connectrpc::client::{ClientConfig, Http2Connection, SharedHttp2Connection};
use korpis_agent::content::ContentStore;
use korpis_core::workload::assignment::IdRange;
use korpis_driver_kit::serve::{bind, serve};
use korpis_driver_oci::containerd::Containerd;
use korpis_driver_oci::service::Driver;
use korpis_driver_oci::validate::Policy;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

const CONTAINERD: &str = "/run/containerd/containerd.sock";
/// The test registry: the development one unless `KORPIS_TEST_REGISTRY` names another.
fn test_registry() -> String {
    std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned())
}

/// busybox, by digest, as `scripts/dev-registry.sh` mirrors it into the test registry: the tests
/// pull nothing from a public registry, whose limits on anonymous pulls make them fail at random.
fn busybox() -> String {
    format!(
        "{}/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e",
        test_registry()
    )
}

type Client = driver::DriverServiceClient<SharedHttp2Connection>;

fn client(socket: &Path) -> Client {
    let authority: http::Uri = "http://localhost".parse().unwrap();
    let conn = Http2Connection::lazy_unix(socket, authority.clone()).shared(16);
    driver::DriverServiceClient::new(
        conn,
        ClientConfig::new(authority).with_protocol(Protocol::Grpc),
    )
}

/// A driver serving on a socket in `dir` to `allowed`; its client.
async fn started(dir: &Path, allowed: BTreeSet<u32>) -> Client {
    let content = dir.join("content");
    ContentStore::open(&content, vec![test_registry()]).unwrap().fetch(&busybox()).await.unwrap();
    let containerd = Containerd::connect(Path::new(CONTAINERD), &content).await.unwrap();
    let socket = dir.join("driver.sock");
    let listener = bind(&socket).unwrap();
    let (d, _watcher) = Driver::new(
        containerd,
        Policy {
            volume_root: dir.join("volumes"),
            identity_root: dir.join("identity"),
            netns_root: dir.join("netns"),
        },
    );
    tokio::spawn(serve(listener, allowed, d, std::future::pending()));
    client(&socket)
}

fn spec(dir: &Path, nonce: &str) -> driver::PreparedSpec {
    let ids = IdRange::of_slot(4001).unwrap();
    driver::PreparedSpec {
        workload: format!("wl_serve{nonce}"),
        intent: format!("in_serve{nonce}"),
        rootfs: busybox(),
        command: vec!["/bin/sh".into(), "-c".into(), "exec sleep 300".into()],
        tier: pb::IsolationTier::ISOLATION_TIER_CONTAINER.into(),
        labels: driver::RuntimeLabels {
            workload: format!("wl_serve{nonce}"),
            intent: format!("in_serve{nonce}"),
            ..Default::default()
        }
        .into(),
        uid_base: ids.base,
        uid_count: ids.count,
        resources: pb::ResourceSpec {
            pids: pb::PidsSpec { limit: 64, ..Default::default() }.into(),
            ..Default::default()
        }
        .into(),
        mounts: vec![driver::Mount {
            source: dir.join("volumes/data").to_string_lossy().into_owned(),
            destination: "/data".into(),
            volume: "data".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

type Events =
    connectrpc::client::ServerStream<hyper::body::Incoming, driver::EventsResponseView<'static>>;

/// Reads events until one says `id` is running.
async fn until_running(events: &mut Events, id: &str) {
    loop {
        let e = events.message::<driver::EventsResponse>().await.unwrap().unwrap();
        let e = e.to_owned_message();
        let state = e.status.as_option().and_then(|s| s.state.as_known());
        if e.runtime_id == id && state == Some(driver::RuntimeState::RUNTIME_STATE_RUNNING) {
            return;
        }
    }
}

fn live() -> bool {
    let _ = rustls::crypto::ring::default_provider().install_default();
    std::os::unix::net::UnixStream::connect(CONTAINERD).is_ok()
}

#[tokio::test]
async fn the_protocol_runs_a_container_and_reports_what_it_does() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("volumes/data")).unwrap();
    let own = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap());
    let c = started(dir.path(), BTreeSet::from([own])).await;
    let info = c.describe(driver::DescribeRequest::default()).await.unwrap().into_owned();
    let info = info.info.as_option().unwrap();
    assert!(info.healthy && info.name == "oci", "{info:?}");

    let mut events = c.events(driver::EventsRequest::default()).await.unwrap();
    let nonce = uuid::Uuid::now_v7().simple().to_string();
    let create =
        driver::CreateRequest { spec: spec(dir.path(), &nonce).into(), ..Default::default() };
    let id = c.create(create).await.unwrap().into_owned().runtime_id;
    c.start(driver::StartRequest { runtime_id: id.clone(), ..Default::default() }).await.unwrap();
    let running = tokio::time::timeout(Duration::from_secs(10), until_running(&mut events, &id));
    running.await.expect("the start was reported as an event");
    let destroy = driver::DestroyRequest { runtime_id: id.clone(), ..Default::default() };
    c.destroy(destroy).await.unwrap();
    let gone = c.inspect(driver::InspectRequest { runtime_id: id, ..Default::default() }).await;
    assert_eq!(gone.unwrap_err().code, connectrpc::ErrorCode::NotFound);
}

#[tokio::test]
async fn a_peer_the_kernel_does_not_allow_is_never_heard() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = started(dir.path(), BTreeSet::from([u32::MAX - 1])).await;
    let refused = c.describe(driver::DescribeRequest::default()).await.unwrap_err();
    assert_eq!(refused.code, connectrpc::ErrorCode::Unavailable, "{refused:?}");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(dir.path().join("driver.sock")).unwrap().permissions(),
    );
    assert_eq!(mode & 0o777, 0o660, "owner and group only");
}
