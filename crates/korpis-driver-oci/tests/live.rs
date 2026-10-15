// SPDX-License-Identifier: AGPL-3.0-or-later

//! The driver against the machine's containerd, when there is one and the test runs as root:
//! busybox, fetched into a content store by the agent's own fetcher, imported, unpacked, and run
//! as a hardened container in the `korpis` namespace, then stopped, started again, and destroyed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

use korpis_agent::content::ContentStore;
use korpis_core::workload::assignment::IdRange;
use korpis_driver_oci::containerd::Containerd;
use korpis_driver_oci::validate::{Policy, validate};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::Duration;

const SOCKET: &str = "/run/containerd/containerd.sock";
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

fn spec(volumes: &Path, nonce: &str) -> driver::PreparedSpec {
    let ids = IdRange::of_slot(4000).unwrap();
    driver::PreparedSpec {
        workload: format!("wl_live{nonce}"),
        intent: format!("in_live{nonce}"),
        rootfs: busybox(),
        command: vec!["/bin/sh".into(), "-c".into(), "id -u > /data/up; exec sleep 300".into()],
        tier: pb::IsolationTier::ISOLATION_TIER_CONTAINER.into(),
        labels: driver::RuntimeLabels {
            workload: format!("wl_live{nonce}"),
            intent: format!("in_live{nonce}"),
            assignment_generation: 3,
            ..Default::default()
        }
        .into(),
        uid_base: ids.base,
        uid_count: ids.count,
        resources: pb::ResourceSpec {
            pids: pb::PidsSpec { limit: 64, ..Default::default() }.into(),
            memory: pb::ReservedLimit { limit: 128 << 20, ..Default::default() }.into(),
            ..Default::default()
        }
        .into(),
        mounts: vec![driver::Mount {
            source: volumes.join("data").to_string_lossy().into_owned(),
            destination: "/data".into(),
            volume: "data".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

async fn until_state(c: &Containerd, id: &str, state: driver::RuntimeState) {
    for _ in 0..100 {
        if c.inspect(id).await.unwrap().state.as_known() == Some(state) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{id} never reached {state:?}");
}

/// What the driver may mount and join, all under `volumes`.
fn policy(volumes: &Path) -> Policy {
    Policy {
        volume_root: volumes.to_owned(),
        identity_root: volumes.join("identity"),
        netns_root: volumes.join("netns"),
    }
}

#[tokio::test]
async fn a_hardened_container_runs_stops_starts_and_is_destroyed() {
    // Only where containerd runs and this process may reach it, which takes root.
    if std::os::unix::net::UnixStream::connect(SOCKET).is_err() {
        return;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let content = dir.path().join("content");
    ContentStore::open(&content, vec![test_registry()]).unwrap().fetch(&busybox()).await.unwrap();
    let volumes = dir.path().join("volumes");
    std::fs::create_dir_all(volumes.join("data")).unwrap();
    let nonce = uuid::Uuid::now_v7().simple().to_string();
    let c = Containerd::connect(Path::new(SOCKET), &content).await.unwrap();
    let valid = validate(spec(&volumes, &nonce), &policy(&volumes)).unwrap();
    let id = c.create(&valid, &policy(&volumes)).await.unwrap();
    assert_eq!(
        c.create(
            &valid,
            &Policy {
                volume_root: volumes.clone(),
                identity_root: volumes.join("identity"),
                netns_root: volumes.join("netns")
            }
        )
        .await
        .unwrap(),
        id,
        "idempotent"
    );
    until_state(&c, &id, driver::RuntimeState::RUNTIME_STATE_CREATED).await;

    c.start(&id).await.unwrap();
    until_state(&c, &id, driver::RuntimeState::RUNTIME_STATE_RUNNING).await;
    let up = volumes.join("data/up");
    for _ in 0..50 {
        if std::fs::read_to_string(&up).is_ok_and(|s| !s.trim().is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(std::fs::read_to_string(&up).unwrap().trim(), "0", "root of its own namespace");
    assert_eq!(std::fs::metadata(&up).unwrap().uid(), 0, "workload-relative ids on disk");
    let found = c.recover().await.unwrap().into_iter().find(|o| o.runtime_id == id).unwrap();
    assert_eq!(found.labels.as_option().unwrap().assignment_generation, 3, "labelled for adoption");
    c.usage(&id).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(c.usage(&id).unwrap().memory_bytes > 0);

    c.stop(&id, 15, Duration::from_secs(5)).await.unwrap();
    until_state(&c, &id, driver::RuntimeState::RUNTIME_STATE_STOPPED).await;
    c.start(&id).await.unwrap();
    until_state(&c, &id, driver::RuntimeState::RUNTIME_STATE_RUNNING).await;
    c.destroy(&id).await.unwrap();
    assert!(c.inspect(&id).await.is_err(), "nothing is left");
    assert!(volumes.join("data/up").exists(), "the volume is never touched");
}
