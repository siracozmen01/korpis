// SPDX-License-Identifier: AGPL-3.0-or-later

//! An exec probe in a microVM on a real node (§4.2 and §5 of docs/04-runtimes.md): its liveness
//! command runs inside the guest, as its entry runs, through the `firecracker` driver's `Exec` and
//! the guest's agent on its vsock device. The guest makes itself sick on its first boot, counted
//! on its volume, so it is healthy, then unhealthy and restarted for its command failing, and
//! healthy again in its second boot. Needs what the microVM test needs, and is skipped without it.

use crate::node::{Node, live};
use crate::recipes::{VERSION, busybox, push};
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

/// A guest that makes `/tmp/sick` twenty seconds into its first boot, and is alive while it is
/// not there.
fn recipe() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: sickens, version: "{VERSION}", description: a guest, license: MIT}}
runtime:
  targets:
    - {{tier: microvm, arch: [amd64], image: "{busybox}"}}
  entry:
    command:
      - /bin/sh
      - -c
      - >-
        n=$(($(cat /data/boots 2>/dev/null || echo 0) + 1)); echo $n > /data/boots; sync;
        echo boot $n; if [ $n -eq 1 ]; then sleep 20; touch /tmp/sick; fi; exec sleep 100000
preset:
  lifecycle: persistent
  console: logs
  volumes:
    - {{name: data, path: /data, size_default: 64Mi}}
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu:    {{min: 100m, recommended: 500m}}
probes:
  liveness: {{kind: exec, command: [/bin/sh, -c, "test ! -e /tmp/sick"], period: 2s, failures: 2}}
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

#[tokio::test]
async fn a_guest_whose_command_fails_inside_it_is_restarted_and_healthy_again() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let Some(node) = Node::join_with_microvms(&cp, "vm-exec").await else {
        eprintln!("this machine cannot run a guest: not run");
        return;
    };
    let agent = node.agent().await;
    let w = cp.declare(&project, "sickens", &push("sickens", recipe()).await).await;
    node.guard(&w);
    health(&cp, &w, pb::HealthState::HEALTH_STATE_HEALTHY).await;
    // Its unhealthy spell lasts only until its restart, which the record shows.
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
