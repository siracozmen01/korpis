// SPDX-License-Identifier: AGPL-3.0-or-later

//! A microVM on a node that runs both drivers (§3 and §6 of docs/04-runtimes.md): the `oci` and
//! `firecracker` drivers serve one agent, a container and a guest are placed on the node side by
//! side, and the guest converges as the container does. Its console is read with `korpis logs`,
//! its endpoint answers through the node's port, its volume, a disk the helper made, keeps what it
//! wrote across a restart, and it sees no more memory than its limit. Needs what the node tests
//! need, and KVM with the pinned Firecracker and guest kernel (`scripts/tools.sh`,
//! `scripts/guest-kernel.sh`); skipped without them.

use crate::network::served;
use crate::node::{Node, host_address, live};
use crate::oci::reading;
use crate::recipes::{VERSION, busybox, push, push_server};
use crate::support::{ControlPlane, converged, mutation};
use korpis_cli::cmd::logs::{Logs, Reading, logs};
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, Instant};

/// A recipe for the microvm tier alone: busybox, counting its boots on its volume, and serving
/// the count and the memory its kernel sees on the port its endpoint declares.
fn recipe() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: guest, version: "{VERSION}", description: a guest, license: MIT}}
runtime:
  targets:
    - {{tier: microvm, arch: [amd64], image: "{busybox}"}}
  entry:
    command:
      - /bin/sh
      - -c
      - >-
        n=$(($(cat /data/boots 2>/dev/null || echo 0) + 1)); echo $n > /data/boots; sync;
        mkdir -p /tmp/www; {{ echo boots $n; grep MemTotal /proc/meminfo; }} > /tmp/www/index.html;
        echo serving boot $n; exec httpd -f -p 8080 -h /tmp/www
preset:
  lifecycle: persistent
  console: logs
  endpoints:
    - {{name: http, protocol: tcp, port: 8080, exposure_default: node_port}}
  volumes:
    - {{name: data, path: /data, size_default: 64Mi}}
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu:    {{min: 100m, recommended: 500m}}
"#
    )
}

/// What `w`'s endpoint `http` answers through the node's port, once it says `wanted`, within two
/// minutes.
async fn answered(cp: &ControlPlane, w: &str, wanted: &str) -> String {
    let status = cp.status(w).await;
    let http = status.endpoints.iter().find(|e| e.name == "http").expect("its endpoint");
    let port = u16::try_from(http.address.as_option().expect("an address").port).unwrap();
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        let said = served((host_address(), port).into()).await.unwrap_or_default();
        if said.contains(wanted) {
            return said;
        }
        assert!(Instant::now() < until, "never answered {wanted:?}; last {said:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Restarts `w` through the API, approving the plan if it asks for that.
async fn restart(cp: &ControlPlane, w: &str) {
    let request = pb::RestartRequest {
        mutation: pb::Mutation { expected_version: Some(1), ..mutation("restart-guest") }.into(),
        workload: w.to_owned(),
        ..Default::default()
    };
    let answer = pb::WorkloadServiceClient::new(cp.http(), cp.config())
        .restart(request)
        .await
        .unwrap()
        .into_owned();
    let plan = answer.plan.into_option().unwrap();
    if plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
        cp.approve(&plan.id).await;
    }
}

#[tokio::test]
async fn a_microvm_beside_a_container_serves_through_the_node_and_keeps_its_volume_and_limit() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let Some(node) = Node::join_with_microvms(&cp, "vm-1").await else {
        eprintln!("this machine cannot run a guest: not run");
        return;
    };
    let agent = node.agent().await;
    let container = cp.declare(&project, "server", &push_server().await).await;
    node.guard(&container);
    let w = cp.declare(&project, "guest", &push("guest", recipe()).await).await;
    converged(&cp, &container).await;
    converged(&cp, &w).await;
    assert!(answered(&cp, &container, "served").await.contains("served"), "the container");

    // Its console, read through the relay as a container's is.
    let tenant = cp.token_for(&["workload.read", "workload.console.read"]).await;
    let reading =
        Reading { follow: true, limit: Some(1), grep: Some("serving".into()), ..reading() };
    let mut printed = Vec::new();
    let asked = Logs { workload: w.clone(), reading };
    logs(&cp.cli(Some(&tenant)), &asked, &mut |line, _| printed.push(line)).await.unwrap();
    assert_eq!(printed, ["serving boot 1"], "what its entry printed on the serial port");

    // Its endpoint, through the node, and the memory its own kernel sees: no more than its limit.
    let said = answered(&cp, &w, "boots 1").await;
    let total = said.lines().find_map(|l| l.strip_prefix("MemTotal:")).expect("its memory");
    let kib: u64 = total.trim().trim_end_matches("kB").trim().parse().unwrap();
    assert!(kib <= 128 * 1024 && kib > 64 * 1024, "{kib} KiB in a guest limited to 128 MiB");

    // Restarted, it finds on its volume what it wrote there.
    restart(&cp, &w).await;
    answered(&cp, &w, "boots 2").await;
    agent.stop().await;
}
