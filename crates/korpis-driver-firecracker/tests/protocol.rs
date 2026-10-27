// SPDX-License-Identifier: AGPL-3.0-or-later

//! The agent with both of Korpis's drivers (§3 of docs/04-runtimes.md): the `oci` and the
//! `firecracker` driver each served as its process serves it, on a socket of its own, and the
//! agent's runtime over both as it runs on a node. Each is described; a `Prepare` reaches the
//! driver its intent names and a driver the node lacks is refused by name; and a guest is created,
//! started, watched, killed, recovered, and destroyed through the agent, every verb reaching the
//! driver that made it. Needs what the runtime tests need and the machine's containerd.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod support;

use korpis_agent::runtime::drivers::Drivers;
use korpis_agent::runtime::process::DriverRuntime;
use korpis_agent::runtime::{Prepared, Runtime, RuntimeError, Sender};
use korpis_driver_kit::serve::{bind, serve};
use korpis_driver_oci::containerd::Containerd;
use korpis_driver_oci::validate::Policy;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};
use support::{Rig, live};

const CONTAINERD: &str = "/run/containerd/containerd.sock";

fn own_uid() -> u32 {
    std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap())
}

/// Both drivers served in `dir`, and the agent's runtime over the two.
async fn node(rig: &Rig) -> Drivers<DriverRuntime> {
    let dir = rig.dir.path();
    let containerd = Containerd::connect(Path::new(CONTAINERD), &dir.join("content")).await;
    let policy = Policy {
        volume_root: dir.join("volumes"),
        shared_roots: Vec::new(),
        block_root: dir.join("block"),
        identity_root: dir.join("identity"),
        netns_root: dir.join("netns"),
        devices: korpis_driver_oci::devices::Specs { dirs: Vec::new(), owner: 0 },
    };
    let (oci, _watcher) = korpis_driver_oci::service::Driver::new(containerd.unwrap(), policy);
    let firecracker = korpis_driver_firecracker::service::Driver::new(rig.machine.clone());
    let allowed = BTreeSet::from([own_uid()]);
    let (oci_socket, fc_socket) = (dir.join("oci.sock"), dir.join("firecracker.sock"));
    let pending = std::future::pending;
    tokio::spawn(serve(bind(&oci_socket).unwrap(), allowed.clone(), oci, pending()));
    tokio::spawn(serve(bind(&fc_socket).unwrap(), allowed, firecracker, pending()));
    let each = vec![DriverRuntime::connect(&oci_socket), DriverRuntime::connect(&fc_socket)];
    Drivers::new(each).await.unwrap()
}

fn recipe(tier: &str, image: &str) -> Vec<u8> {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: sleeper, version: "1.0.0", description: sleeps, license: MIT}}
runtime:
  targets:
    - {{tier: {tier}, arch: [amd64, arm64], image: "{image}"}}
  entry: {{command: [/bin/sh, -c, "exec sleep 300"]}}
"#
    )
    .into_bytes()
}

fn intent(driver: &str, tier: pb::IsolationTier) -> pb::Intent {
    let body = pb::IntentBody {
        console: pb::ConsoleMode::CONSOLE_MODE_NONE.into(),
        runtime: pb::RuntimeSpec { driver: driver.into(), tier: tier.into(), ..Default::default() }
            .into(),
        ..Default::default()
    };
    pb::Intent { id: "in_both".into(), body: body.into(), ..Default::default() }
}

async fn state_once(node: &Drivers<DriverRuntime>, id: &str, state: driver::RuntimeState) {
    let start = Instant::now();
    loop {
        let s = node.inspect(id).await.unwrap();
        if s.state.as_known() == Some(state) {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "{id} never reached {state:?}: {s:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Each driver's `Prepare` answers the intent that names it; a driver the node lacks is refused.
async fn prepares_route_by_name(node: &Drivers<DriverRuntime>, image: &str) {
    let microvm = pb::IsolationTier::ISOLATION_TIER_MICROVM;
    let container = pb::IsolationTier::ISOLATION_TIER_CONTAINER;
    for (name, tier, word) in [("firecracker", microvm, "microvm"), ("oci", container, "container")]
    {
        let prepared = node.prepare("wl_both", &intent(name, tier), &recipe(word, image)).await;
        let Ok(Prepared::Spec(spec)) = prepared else { panic!("{name}: {prepared:?}") };
        assert_eq!((spec.driver.as_str(), spec.tier.as_known()), (name, Some(tier)));
    }
    let absent = node.prepare("wl_both", &intent("qemu", microvm), &recipe("microvm", image)).await;
    let Ok(Prepared::Rejected(r)) = absent else { panic!("no qemu here: {absent:?}") };
    assert!(r.detail.contains("qemu"), "the rejection names the driver: {r:?}");
}

#[tokio::test]
async fn the_agent_routes_each_verb_to_the_driver_that_serves_it() {
    if !live() || std::os::unix::net::UnixStream::connect(CONTAINERD).is_err() {
        return;
    }
    let rig = Rig::new();
    let node = node(&rig).await;
    let names: Vec<_> = node.drivers().await.unwrap().into_iter().map(|d| d.name).collect();
    assert_eq!(names, ["oci", "firecracker"], "every driver is reported on the node");
    prepares_route_by_name(&node, &rig.image).await;

    let mut changes = node.changes();
    let id = node.create(&rig.spec("wl_both", "while true; do sleep 1; done")).await.unwrap();
    assert!(id.starts_with("fc-"), "made by the firecracker driver: {id}");
    node.start(&id).await.unwrap();
    state_once(&node, &id, driver::RuntimeState::RUNTIME_STATE_RUNNING).await;
    let heard = tokio::time::timeout(Duration::from_secs(5), changes.recv()).await;
    assert_eq!(heard.unwrap().unwrap(), id, "a guest's change reaches the agent");
    let alex = Sender { by: "user:alex", session: "s1" };
    let resized = node.resize_console(&id, alex, (80, 24)).await;
    assert!(resized.is_err(), "a console of logs has no terminal to resize: {resized:?}");
    // Routed to the driver, and through it to the guest's agent, which sends it by name.
    node.signal(&id, "SIGWINCH").await.unwrap();
    let signalled = node.signal(&id, "SIGSEGV").await;
    assert!(matches!(signalled, Err(RuntimeError::Failed(_))), "undeclared: {signalled:?}");

    let again = Drivers::new(vec![
        DriverRuntime::connect(&rig.dir.path().join("oci.sock")),
        DriverRuntime::connect(&rig.dir.path().join("firecracker.sock")),
    ])
    .await
    .unwrap();
    let found = again.recover().await.unwrap();
    assert!(found.iter().any(|o| o.runtime_id == id), "an agent restarted adopts the guest");
    again.kill(&id).await.unwrap();
    state_once(&again, &id, driver::RuntimeState::RUNTIME_STATE_STOPPED).await;
    again.destroy(&id).await.unwrap();
    let gone = again.inspect(&id).await;
    assert!(matches!(gone, Err(RuntimeError::NotFound(_))), "{gone:?}");
}
