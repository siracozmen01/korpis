// SPDX-License-Identifier: AGPL-3.0-or-later

//! An exec probe on a real node (§5 of docs/04-runtimes.md): a container whose liveness probe
//! runs `test ! -e /tmp/sick` inside it, through the `oci` driver's `Exec`, which containerd runs
//! in the container's namespaces as its own user. Healthy while the command succeeds; once the
//! file is there, unhealthy and restarted for it; and healthy again in its new life, whose
//! entry removes the file.

use crate::dependencies::running;
use crate::node::{Node, live};
use crate::recipes::{VERSION, busybox, push};
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

/// A container that sleeps, and is alive while `/tmp/sick` is not there.
fn sleeper() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: execd, version: "{VERSION}", description: is probed, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
  entry:
    command: [/bin/sh, -c, "rm -f /tmp/sick; exec sleep 100000"]
preset:
  lifecycle: persistent
  console: logs
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu:    {{min: 50m, recommended: 100m}}
probes:
  liveness: {{kind: exec, command: [/bin/sh, -c, "test ! -e /tmp/sick"], period: 1s, failures: 2}}
"#
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

/// Runs `script` as root of the user namespace of the process `pid`, in its mounts.
fn within(pid: u32, script: &str) {
    let ran = std::process::Command::new("nsenter")
        .args(["-t", &pid.to_string(), "-U", "-m", "sh", "-c", script])
        .status()
        .unwrap();
    assert!(ran.success(), "{script}");
}

#[tokio::test]
async fn a_command_that_fails_inside_its_container_restarts_it() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "exec-1").await;
    let agent = node.agent().await;
    let w = cp.declare(&project, "execd", &push("execd", sleeper()).await).await;
    node.guard(&w);

    health(&cp, &w, pb::HealthState::HEALTH_STATE_HEALTHY).await;
    let pid = running(&w).await;
    within(pid, "touch /tmp/sick");
    health(&cp, &w, pb::HealthState::HEALTH_STATE_UNHEALTHY).await;
    let restarts = "SELECT count(*) FROM effect WHERE action = 'node.restart' \
                    AND after::text LIKE '%unhealthy%' AND after::text LIKE '%exec%'";
    for _ in 0..240 {
        if cp.scalar::<i64>(restarts).await >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(cp.scalar::<i64>(restarts).await >= 1, "restarted for its failing command");
    health(&cp, &w, pb::HealthState::HEALTH_STATE_HEALTHY).await;
    cp.retire(&w).await;
    agent.stop().await;
}
