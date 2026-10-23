// SPDX-License-Identifier: AGPL-3.0-or-later

//! A heartbeat probe on a real node (§5 of docs/04-runtimes.md): a container runs `korpis
//! heartbeat --every 1s`, which says on the identity socket its node mounts into it that it is
//! alive, and its liveness probe allows three seconds of silence. Healthy while it beats; once its
//! process is stopped from the host, silent, so unhealthy and restarted; and healthy again in its
//! new life.

use crate::dependencies::running;
use crate::image::{KORPIS, push_image};
use crate::node::{Node, live};
use crate::recipes::{VERSION, push, test_registry};
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

/// A workload whose only work is to beat, and whose liveness is that it does.
fn beating(image: &str) -> String {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: beating, version: "{VERSION}", description: beats, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{image}"}}
  entry:
    command: [{entry}, heartbeat, --every, 1s]
preset:
  lifecycle: persistent
  console: logs
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu:    {{min: 50m, recommended: 100m}}
probes:
  liveness: {{kind: heartbeat, max_interval: 3s, period: 1s, failures: 1}}
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
async fn a_container_that_stops_beating_is_restarted_and_beats_again() {
    if !live() {
        return;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    // Its image before any node runs: building it under a node would starve its lease.
    let image = push_image(&test_registry(), KORPIS).await;
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "heartbeat-1").await;
    let agent = node.agent().await;
    let w = cp.declare(&project, "beating", &push("beating", beating(&image)).await).await;
    node.guard(&w);

    health(&cp, &w, pb::HealthState::HEALTH_STATE_HEALTHY).await;
    let pid = running(&w).await;
    let stopped = std::process::Command::new("kill").args(["-STOP", &pid.to_string()]).status();
    assert!(stopped.unwrap().success(), "its process stopped");
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
    assert_ne!(running(&w).await, pid, "beating again in a new process");
    cp.retire(&w).await;
    agent.stop().await;
}
