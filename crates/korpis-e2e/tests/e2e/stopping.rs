// SPDX-License-Identifier: AGPL-3.0-or-later

//! A recipe's `pre_stop` hook on a real node (§3 of docs/09-recipes.md, Finding 87 of
//! docs/23-walkthroughs.md): software that saves and exits only when told `stop` on its console,
//! and that ignores `SIGTERM` as a container's first process with no handler does, is restarted.
//! The node writes the hook's data to its console first, it exits by itself, and the restart
//! completes well inside the thirty seconds a signal and its grace would have taken.

use crate::node::{Node, live};
use crate::recipes::{VERSION, busybox, push};
use crate::support::{ControlPlane, converged, mutation};
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, Instant};

/// Software that answers its console and exits when it reads `stop`, saying so first; it has no
/// handler for `SIGTERM`, which its kernel then ignores in the container's first process.
fn stopper() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: stopper, version: "{VERSION}", description: stops when told, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
  entry:
    command:
      - /bin/sh
      - -c
      - 'echo ready; while read line; do [ "$line" = stop ] && echo saved && exit 0; done'
preset:
  lifecycle: persistent
  console: interactive
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu:    {{min: 100m, recommended: 100m}}
lifecycle_hooks:
  pre_stop: {{kind: console_write, data: "stop\n", wait_for_exit: 60s}}
"#
    )
}

/// Waits until `sql` counts `n`, for up to `patience`.
async fn counted(cp: &ControlPlane, sql: &'static str, n: i64, patience: Duration) {
    let until = Instant::now() + patience;
    while Instant::now() < until {
        if cp.scalar::<i64>(sql).await == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{sql} never reached {n}: {}", cp.scalar::<i64>(sql).await);
}

#[tokio::test]
async fn a_restart_asks_the_software_to_stop_and_completes_inside_the_grace() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "fra-4").await;
    let agent = node.agent().await;
    let w = cp.declare(&project, "stopper", &push("stopper", stopper()).await).await;
    node.guard(&w);
    converged(&cp, &w).await;
    let applied = "SELECT count(*) FROM plan WHERE status = 'applied'";
    let before = cp.scalar::<i64>(applied).await;

    let asked = Instant::now();
    let restart = pb::RestartRequest {
        mutation: pb::Mutation { expected_version: Some(1), ..mutation("restart") }.into(),
        workload: w.clone(),
        ..Default::default()
    };
    let answer = pb::WorkloadServiceClient::new(cp.http(), cp.config())
        .restart(restart)
        .await
        .unwrap()
        .into_owned();
    let plan = answer.plan.into_option().unwrap();
    if plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
        cp.approve(&plan.id).await;
    }
    let restarted = "SELECT count(*) FROM effect WHERE action = 'node.pre_stop' \
                     AND outcome = 'succeeded'";
    counted(&cp, restarted, 1, Duration::from_secs(60)).await;
    while cp.scalar::<i64>(applied).await == before {
        assert!(asked.elapsed() < Duration::from_secs(60), "the restart was never applied");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let took = asked.elapsed();
    // A signal its first process ignores, and the grace, would have taken thirty seconds.
    assert!(took < Duration::from_secs(25), "it stopped when asked, in {took:?}");
    converged(&cp, &w).await;
    agent.stop().await;
}
