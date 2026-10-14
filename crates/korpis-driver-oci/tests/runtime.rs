// SPDX-License-Identifier: AGPL-3.0-or-later

//! The agent's side of the driver process: its `Runtime` adapter over the socket, against the
//! machine's containerd when there is one and the test runs as root. A thin recipe is prepared,
//! built, and started as the reconciler would; a crash reaches the agent as a change before any
//! poll; the object is read back as adoption would; and it is destroyed, its console stream
//! written by the stream writer and sealed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_agent::content::ContentStore;
use korpis_agent::runtime::process::DriverRuntime;
use korpis_agent::runtime::{Prepared, Runtime, RuntimeError};
use korpis_core::workload::assignment::IdRange;
use korpis_core::workload::stream::StreamKind;
use korpis_driver_oci::containerd::Containerd;
use korpis_driver_oci::containerd::stream::Streams;
use korpis_driver_oci::serve::{bind, serve};
use korpis_driver_oci::service::Driver;
use korpis_driver_oci::validate::Policy;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use korpis_stream::changes::{self, Change};
use korpis_stream::dir::StreamDir;
use korpis_stream::read::read;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const CONTAINERD: &str = "/run/containerd/containerd.sock";
const BUSYBOX: &str = "docker.io/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e";

fn recipe() -> Vec<u8> {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: sleeper, version: "1.0.0", description: sleeps, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{BUSYBOX}"}}
  entry: {{command: [/bin/sh, -c, "echo $GREETING > /data/said; echo said $GREETING; exec sleep 300"]}}
config:
  fields:
    - {{key: greeting, type: string, default: hi, deliver: env, deliver_as: GREETING}}
"#
    )
    .into_bytes()
}

fn intent(nonce: &str) -> pb::Intent {
    let hello = pb::Value {
        kind: Some(pb::value::Kind::StringValue("hello".into())),
        ..Default::default()
    };
    let body = pb::IntentBody {
        console: pb::ConsoleMode::CONSOLE_MODE_LOGS.into(),
        runtime: pb::RuntimeSpec {
            tier: pb::IsolationTier::ISOLATION_TIER_CONTAINER.into(),
            ..Default::default()
        }
        .into(),
        volumes: vec![pb::VolumeSpec {
            name: "data".into(),
            path: "/data".into(),
            ..Default::default()
        }],
        config: [("greeting".to_owned(), hello)].into(),
        ..Default::default()
    };
    pb::Intent { id: format!("in_rt{nonce}"), body: body.into(), ..Default::default() }
}

/// A driver serving in `dir` to this process's user; the agent's runtime over its socket.
async fn started(dir: &Path) -> DriverRuntime {
    let content = dir.join("content");
    ContentStore::open(&content, Vec::new()).unwrap().fetch(BUSYBOX).await.unwrap();
    let containerd = Containerd::connect(Path::new(CONTAINERD), &content).await.unwrap();
    let socket = dir.join("driver.sock");
    let listener = bind(&socket).unwrap();
    let streams = Streams { writer: writer(), state: dir.join("driver") };
    let (d, _watcher) = Driver::new(
        containerd.with_streams(streams),
        Policy { volume_root: dir.join("volumes"), identity_root: dir.join("identity") },
    );
    let own = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap());
    tokio::spawn(serve(listener, BTreeSet::from([own]), d, std::future::pending()));
    DriverRuntime::connect(&socket)
}

/// The spec the reconciler would build: prepared, its volume placed, labelled, and mapped.
async fn built(rt: &DriverRuntime, dir: &Path, nonce: &str) -> driver::PreparedSpec {
    let workload = format!("wl_rt{nonce}");
    let Prepared::Spec(spec) = rt.prepare(&workload, &intent(nonce), &recipe()).await.unwrap()
    else {
        panic!("the thin recipe was refused");
    };
    let mut spec = *spec;
    spec.mounts[0].source = dir.join("volumes/data").to_string_lossy().into_owned();
    let console = dir.join("volumes").join(StreamKind::Console.volume(&workload));
    spec.stream.as_option_mut().expect("a console of logs is captured").dir =
        console.to_string_lossy().into_owned();
    spec.labels = driver::RuntimeLabels {
        workload,
        intent: spec.intent.clone(),
        assignment_generation: 7,
        ..Default::default()
    }
    .into();
    let ids = IdRange::of_slot(4002).unwrap();
    (spec.uid_base, spec.uid_count) = (ids.base, ids.count);
    spec
}

async fn state_of(rt: &DriverRuntime, id: &str) -> Option<driver::RuntimeState> {
    rt.inspect(id).await.unwrap().state.as_known()
}

#[tokio::test]
async fn the_agent_runs_a_thin_recipe_through_the_driver_process() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    if std::os::unix::net::UnixStream::connect(CONTAINERD).is_err() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("volumes/data")).unwrap();
    let rt = started(dir.path()).await;
    assert!(rt.describe().await.unwrap().healthy);
    let mut changes = rt.changes();
    let nonce = uuid::Uuid::now_v7().simple().to_string();
    let id = rt.create(&built(&rt, dir.path(), &nonce).await).await.unwrap();
    rt.start(&id).await.unwrap();
    let said = dir.path().join("volumes/data/said");
    for _ in 0..50 {
        if std::fs::read_to_string(&said).is_ok_and(|s| !s.trim().is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(std::fs::read_to_string(&said).unwrap().trim(), "hello", "the configured value");

    rt.signal(&id, "SIGKILL").await.unwrap();
    let crash = tokio::time::timeout(Duration::from_secs(10), async {
        while state_of(&rt, &id).await != Some(driver::RuntimeState::RUNTIME_STATE_STOPPED) {
            changes.recv().await.unwrap();
        }
    });
    crash.await.expect("the crash reached the agent as a change");
    let found = rt.recover().await.unwrap().into_iter().find(|o| o.runtime_id == id).unwrap();
    assert_eq!(found.labels.as_option().unwrap().assignment_generation, 7);

    rt.destroy(&id).await.unwrap();
    assert!(matches!(rt.inspect(&id).await, Err(RuntimeError::NotFound(_))));
    let console = dir.path().join(format!("volumes/wl_rt{nonce}.stream_console"));
    let sealed = tokio::time::timeout(Duration::from_secs(15), sealed_in(&console));
    let sealed = sealed.await.expect("the writer sealed the console when the task went");
    let records = read(&StreamDir::new(&console), 0, 10).unwrap().records;
    let said: Vec<_> = records.iter().map(|r| String::from_utf8_lossy(&r.body)).collect();
    assert_eq!(said, ["said hello"], "what the workload printed, in its stream");
    assert_eq!(sealed.end_offset, 1);
}

/// The last change recorded in the stream in `dir`, once it is a seal.
async fn sealed_in(dir: &Path) -> korpis_stream::segment::Sealed {
    loop {
        if let Some((Change::Sealed(s), _)) = changes::read_from(dir, 0).unwrap().pop() {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The stream writer, built where this test was if it is not already.
fn writer() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(Path::parent).unwrap();
    let target = profile_dir.parent().unwrap();
    let profile = match profile_dir.file_name().unwrap().to_str().unwrap() {
        "debug" => "dev",
        other => other,
    };
    let built = Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", "korpis-stream", "--bin", "korpis-stream-writer"])
        .args(["--profile", profile, "--target-dir"])
        .arg(target)
        .status()
        .unwrap();
    assert!(built.success(), "korpis-stream-writer did not build");
    profile_dir.join("korpis-stream-writer")
}
