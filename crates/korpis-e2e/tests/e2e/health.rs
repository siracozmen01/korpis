// SPDX-License-Identifier: AGPL-3.0-or-later

//! Health probes on a real node (Rule K-16, §5 of docs/04-runtimes.md): a server whose recipe
//! declares an HTTP readiness and liveness probe is judged by the node from inside its network,
//! a web application that requires it is created only once it is ready (§5 of
//! docs/07-networking.md), and when it stops answering its liveness probe it is reported
//! unhealthy and restarted, keeping its volume. The test decides what the server answers by
//! writing into its root, inside its namespaces.

use crate::dependencies::running;
use crate::node::{Node, live};
use crate::recipes::{VERSION, busybox, push, push_server};
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::dependency::Target;
use std::time::Duration;

/// A server counting its boots on its volume, which answers `/ready` once that file is written
/// and `/alive` while that one is there.
fn pulse() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: pulse, version: "{VERSION}", description: answers, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
  entry:
    command:
      - /bin/sh
      - -c
      - >-
        n=$(($(cat /data/boots 2>/dev/null || echo 0) + 1)); echo $n > /data/boots;
        mkdir -p /tmp/www; echo alive > /tmp/www/alive; exec httpd -f -p 8080 -h /tmp/www
preset:
  lifecycle: persistent
  console: logs
  endpoints:
    - {{name: http, protocol: tcp, port: 8080, exposure_default: node_port}}
  volumes:
    - {{name: data, path: /data, size_default: 16Mi}}
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu:    {{min: 50m, recommended: 100m}}
probes:
  readiness: {{kind: http, endpoint: http, path: /ready, period: 1s, failures: 1}}
  liveness:  {{kind: http, endpoint: http, path: /alive, period: 1s, failures: 2}}
"#
    )
}

/// What the node last observed of `w`, once `pick` holds of it, within three minutes.
async fn observed(
    cp: &ControlPlane,
    w: &str,
    pick: impl Fn(&pb::Observation) -> bool,
) -> pb::Observation {
    for _ in 0..1800 {
        let s = cp.status(w).await;
        if let Some(o) = s.observation.as_option().filter(|o| pick(o)) {
            return o.clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{w} never reached the observation awaited: {:?}", cp.status(w).await.observation);
}

fn health_is(o: &pb::Observation, h: pb::HealthState) -> bool {
    o.health.as_known() == Some(h)
}

/// The process id of `w`'s running task once it is another than `pid`, within three minutes.
async fn restarted(w: &str, pid: u32) -> u32 {
    for _ in 0..360 {
        let now = running(w).await;
        if now != pid {
            return now;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("{w} was never restarted");
}

/// Runs `script` as root of the user namespace of the process `pid`, in its mounts: a file the
/// host's root wrote would belong to an id the namespace does not map.
fn within(pid: u32, script: &str) {
    let ran = std::process::Command::new("nsenter")
        .args(["-t", &pid.to_string(), "-U", "-m", "sh", "-c", script])
        .status()
        .unwrap();
    assert!(ran.success(), "{script}");
}

/// The path of `file` in the root of the process `pid`.
fn inside(pid: u32, file: &str) -> String {
    format!("/proc/{pid}/root{file}")
}

#[tokio::test]
async fn a_dependent_waits_for_readiness_and_a_server_that_stops_answering_is_restarted() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "health-1").await;
    let agent = node.agent().await;
    let db = cp.declare(&project, "db", &push("pulse", pulse()).await).await;
    node.guard(&db);
    let dependency = pb::Dependency {
        name: "db".into(),
        target: Some(Target::Workload(db.clone())),
        required: true,
        ..Default::default()
    };
    let body = pb::IntentBody { dependencies: vec![dependency], ..Default::default() };
    let web = cp.declare_body(&project, "web", &push_server().await, body).await;
    node.guard(&web);

    // Running, alive, and not ready: healthy, its endpoint not ready, its dependent waiting.
    let pid = running(&db).await;
    let healthy = |o: &pb::Observation| health_is(o, pb::HealthState::HEALTH_STATE_HEALTHY);
    let o = observed(&cp, &db, healthy).await;
    assert!(o.endpoints.iter().all(|e| !e.ready), "not ready: {:?}", o.endpoints);
    let waiting = observed(&cp, &web, |o| o.reason_detail.contains("dependency db")).await;
    assert_eq!(waiting.state.as_known(), Some(pb::ObservedState::OBSERVED_STATE_STARTING));

    within(pid, "echo ready > /tmp/www/ready");
    observed(&cp, &db, |o| o.endpoints.iter().all(|e| e.ready)).await;
    running(&web).await;

    // It stops answering its liveness probe: unhealthy, and restarted with its volume.
    within(pid, "rm /tmp/www/alive");
    observed(&cp, &db, |o| health_is(o, pb::HealthState::HEALTH_STATE_UNHEALTHY)).await;
    let pid_again = restarted(&db, pid).await;
    let boots = std::fs::read_to_string(inside(pid_again, "/data/boots")).unwrap();
    assert_eq!(boots.trim(), "2", "started again on the volume it had");
    observed(&cp, &db, healthy).await;
    let restarts = "SELECT count(*) FROM effect WHERE action = 'node.restart' \
                    AND after::text LIKE '%unhealthy%' AND after::text LIKE '%http%'";
    for _ in 0..300 {
        if cp.scalar::<i64>(restarts).await >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(cp.scalar::<i64>(restarts).await >= 1, "recorded with its cause");
    for w in [&web, &db] {
        cp.retire(w).await;
    }
    agent.stop().await;
}
