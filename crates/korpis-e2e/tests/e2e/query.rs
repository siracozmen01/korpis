// SPDX-License-Identifier: AGPL-3.0-or-later

//! A query probe on a real node (§5 of docs/04-runtimes.md, §5 of docs/16-extensions.md): an
//! extension written with the SDK, running in a container on the node, provides the
//! `korpis-test/echo` query protocol, and a server whose liveness probe queries it is judged by it.
//! The node runs each round from inside the server's namespace and asks the control plane what to
//! send next; the provider never reaches the server. Healthy while it answers `pong`; unhealthy,
//! and restarted, once it stops; and once the provider is gone, unjudged: its health unknown, and
//! nothing restarted for it.

use crate::dependencies::running;
use crate::extension::{install, pinged_as};
use crate::image::{TEST_EXTENSION, push_image};
use crate::node::{Node, live};
use crate::recipes::{VERSION, busybox, push};
use crate::support::{ControlPlane, mutation};
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

/// The extension's document: the test extension, serving the `korpis-test/echo` protocol.
fn provider(image: &str) -> String {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Extension
metadata: {{name: echo-health, version: "{VERSION}", description: judges, license: Apache-2.0}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{image}"}}
  entry:
    command: [{entry}, --health, korpis-test/echo]
preset:
  lifecycle: persistent
  console: logs
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu: {{min: 50m, recommended: 100m}}
extension:
  api_version: korpis.extension.v1
  requires: [provider.serve, provider.health]
  provides:
    - {{type: health, name: korpis-test/echo}}
"#,
        entry = TEST_EXTENSION.entry(),
    )
}

/// A server answering `pong` on port 7000 unless `/tmp/quiet` is there, its liveness probe the
/// echo query, and counting its boots on its volume.
fn server() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: pong, version: "{VERSION}", description: answers pong, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
  entry:
    command:
      - /bin/sh
      - -c
      - >-
        n=$(($(cat /data/boots 2>/dev/null || echo 0) + 1)); echo $n > /data/boots; rm -f /tmp/quiet;
        while true; do if [ -f /tmp/quiet ]; then sleep 1; else echo pong | nc -l -p 7000; fi; done
preset:
  lifecycle: persistent
  console: logs
  endpoints:
    - {{name: echo, protocol: tcp, port: 7000, exposure_default: node_port}}
  volumes:
    - {{name: data, path: /data, size_default: 16Mi}}
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu:    {{min: 50m, recommended: 100m}}
probes:
  liveness: {{kind: query, protocol: korpis-test/echo, endpoint: echo, period: 5s, failures: 2}}
"#
    )
}

/// The node's last observation of `w`, once `pick` holds of it, within three minutes.
async fn observed(
    cp: &ControlPlane,
    w: &str,
    pick: impl Fn(&pb::Observation) -> bool,
) -> pb::Observation {
    for _ in 0..360 {
        let s = cp.status(w).await;
        if let Some(o) = s.observation.as_option().filter(|o| pick(o)) {
            return o.clone();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("{w} never reached the observation awaited: {:?}", cp.status(w).await.observation);
}

fn health_is(h: pb::HealthState) -> impl Fn(&pb::Observation) -> bool {
    move |o| o.health.as_known() == Some(h)
}

/// Runs `script` as root of the user namespace of the process `pid`, in its mounts.
fn within(pid: u32, script: &str) {
    let ran = std::process::Command::new("nsenter")
        .args(["-t", &pid.to_string(), "-U", "-m", "sh", "-c", script])
        .status()
        .unwrap();
    assert!(ran.success(), "{script}");
}

/// How many times the node restarted a workload for its health.
async fn restarts(cp: &ControlPlane) -> i64 {
    let sql = "SELECT count(*) FROM effect WHERE action = 'node.restart' \
               AND after::text LIKE '%unhealthy%' AND after::text LIKE '%query%'";
    cp.scalar::<i64>(sql).await
}

#[tokio::test]
async fn a_query_provider_judges_a_server_it_never_reaches_and_its_absence_restarts_nothing() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "query-1").await;
    let agent = node.agent().await;
    let image = push_image(&crate::recipes::test_registry(), TEST_EXTENSION).await;
    let extension = install(&cp, &push("echo-health", provider(&image)).await, &project).await;
    node.guard(&extension.workload);
    let health = pb::ProviderType::PROVIDER_TYPE_HEALTH;
    pinged_as(&cp, health, "korpis-test/echo").await;
    let w = cp.declare(&project, "pong", &push("pong", server()).await).await;
    node.guard(&w);

    observed(&cp, &w, health_is(pb::HealthState::HEALTH_STATE_HEALTHY)).await;
    let pid = running(&w).await;
    within(pid, "touch /tmp/quiet");
    observed(&cp, &w, health_is(pb::HealthState::HEALTH_STATE_UNHEALTHY)).await;
    for _ in 0..240 {
        if restarts(&cp).await >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(restarts(&cp).await >= 1, "restarted for its query probe");
    observed(&cp, &w, health_is(pb::HealthState::HEALTH_STATE_HEALTHY)).await;

    // The provider gone: unjudged, its health unknown, and no restart for it however long.
    let gone = pb::UninstallExtensionRequest {
        mutation: mutation("uninstall").into(),
        id: extension.id.clone(),
        ..Default::default()
    };
    pb::ExtensionServiceClient::new(cp.http(), cp.config())
        .uninstall_extension(gone)
        .await
        .unwrap();
    observed(&cp, &w, health_is(pb::HealthState::HEALTH_STATE_UNKNOWN)).await;
    let before = restarts(&cp).await;
    within(running(&w).await, "touch /tmp/quiet");
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert_eq!(restarts(&cp).await, before, "an unjudged probe restarts nothing");
    cp.retire(&w).await;
    agent.stop().await;
}
