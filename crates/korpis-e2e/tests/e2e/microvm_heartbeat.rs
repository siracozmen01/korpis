// SPDX-License-Identifier: AGPL-3.0-or-later

//! A heartbeat probe on a microVM on a real node (§5 and §6 of docs/04-runtimes.md): the guest
//! runs `korpis heartbeat --every 1s`, which says on `/run/korpis/identity.sock` inside the guest
//! that it is alive, relayed over vsock to the workload's socket its node serves on the host, and
//! its liveness probe allows three seconds of silence after ten for its boot. Healthy while it
//! beats; once its Firecracker is stopped from the host, as a hung guest is, silent, so unhealthy
//! and restarted, though neither its agent nor Ctrl+Alt+Del can reach it; and healthy again in its
//! new boot. Needs what the microVM test needs, and is skipped without
//! it.

use crate::image::{KORPIS, push_image};
use crate::node::{Node, live};
use crate::recipes::{VERSION, push, test_registry};
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

/// A guest whose only work is to beat, and whose liveness is that it does, allowing its boot.
fn beating(image: &str) -> String {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: beating-guest, version: "{VERSION}", description: beats, license: MIT}}
runtime:
  targets:
    - {{tier: microvm, arch: [amd64], image: "{image}"}}
  entry:
    command: [{entry}, heartbeat, --every, 1s]
preset:
  lifecycle: persistent
  console: logs
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu:    {{min: 100m, recommended: 500m}}
probes:
  liveness: {{kind: heartbeat, max_interval: 3s, period: 1s, failures: 1, initial_delay: 10s}}
"#,
        entry = KORPIS.entry(),
    )
}

/// Whether `w`'s health is `h`, within three minutes.
async fn health(cp: &ControlPlane, w: &str, h: pb::HealthState) {
    for _ in 0..360 {
        let s = cp.status(w).await;
        if s.observation.as_option().is_some_and(|o| o.health.as_known() == Some(h)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("{w} never became {h:?}: {:?}", cp.status(w).await.observation);
}

#[tokio::test]
async fn a_guest_that_stops_beating_is_restarted_and_beats_again() {
    if !live() {
        return;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    // Its image before any node runs: building it under a node would starve its lease.
    let image = push_image(&test_registry(), KORPIS).await;
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let Some(node) = Node::join_with_microvms(&cp, "microvm-beat-1").await else { return };
    let agent = node.agent().await;
    let w = cp.declare(&project, "beating", &push("beating-guest", beating(&image)).await).await;
    node.guard(&w);

    health(&cp, &w, pb::HealthState::HEALTH_STATE_HEALTHY).await;
    let hung = node.firecrackers();
    assert_eq!(hung.len(), 1, "one guest");
    for pid in &hung {
        let stopped = std::process::Command::new("kill").args(["-STOP", &pid.to_string()]).status();
        assert!(stopped.unwrap().success(), "its Firecracker stopped");
    }
    health(&cp, &w, pb::HealthState::HEALTH_STATE_UNHEALTHY).await;
    let restarts = "SELECT count(*) FROM effect WHERE action = 'node.restart' \
                    AND after::text LIKE '%unhealthy%' AND after::text LIKE '%heartbeat%'";
    for _ in 0..240 {
        if cp.scalar::<i64>(restarts).await >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(cp.scalar::<i64>(restarts).await >= 1, "restarted for its silence");
    health(&cp, &w, pb::HealthState::HEALTH_STATE_HEALTHY).await;
    assert_ne!(node.firecrackers(), hung, "beating again in a new guest");
    cp.retire(&w).await;
    agent.stop().await;
}
