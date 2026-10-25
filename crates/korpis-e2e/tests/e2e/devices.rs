// SPDX-License-Identifier: AGPL-3.0-or-later

//! A device given to a workload on a real node (§6 of docs/05-scheduling.md, §4.2 of
//! docs/04-runtimes.md). The device is a stand-in: a spec file of the test's own naming the
//! host's `/dev/zero` as `/dev/korpis-standin`, which no container has otherwise. The agent
//! reports it, the first workload asking for it holds it and reads it inside its container, user
//! namespace and all, with the environment its file gives; a second asking for it in
//! `passthrough` is not placed while the first holds it, and is placed once the first is
//! deleted. No real GPU is given here: this machine's is never passed through.

use crate::node::{Node, STANDIN, live};
use crate::oci::reading;
use crate::recipes::{VERSION, busybox, push};
use crate::support::{ControlPlane, converged};
use korpis_cli::cmd::logs::{Logs, Reading, logs};
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

/// A workload that says how many bytes it read from the stand-in, and what its file told it.
fn reader() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: reader, version: "{VERSION}", description: reads a device, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
  entry:
    command:
      - /bin/sh
      - -c
      - >-
        echo "read $(head -c 4 /dev/korpis-standin | wc -c | tr -d ' ') $STANDIN";
        exec sleep 100000
preset:
  lifecycle: persistent
  console: logs
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu:    {{min: 50m, recommended: 100m}}
"#
    )
}

/// The stand-in's spec file, as a vendor's toolkit would write one, in `node`'s directory.
fn standin(node: &Node) {
    let text = format!(
        r#"{{"cdiVersion":"0.6.0","kind":"{STANDIN}","devices":[{{"name":"a",
            "containerEdits":{{"deviceNodes":[{{"path":"/dev/korpis-standin",
                                                "hostPath":"/dev/zero"}}],
                               "env":["STANDIN=given"]}}}}]}}"#
    );
    std::fs::write(node.cdi.join("standin.json"), text).unwrap();
}

/// One serial device, in `passthrough`.
fn asking() -> pb::IntentBody {
    let serial = pb::DeviceRequest {
        class: pb::DeviceClass::DEVICE_CLASS_SERIAL.into(),
        count: 1,
        mode: pb::DeviceMode::DEVICE_MODE_PASSTHROUGH.into(),
        ..Default::default()
    };
    pb::IntentBody { devices: vec![serial], ..Default::default() }
}

/// The first line workload `w` printed, read as `tenant`.
async fn printed(cp: &ControlPlane, tenant: &str, w: &str) -> Vec<String> {
    let reading = Reading { follow: true, limit: Some(1), ..reading() };
    let asked = Logs { workload: w.to_owned(), reading };
    let mut lines = Vec::new();
    logs(&cp.cli(Some(tenant)), &asked, &mut |line, _| lines.push(line)).await.unwrap();
    lines
}

/// Why no node took workload `w`, once the scheduler has said.
async fn unplaced(cp: &ControlPlane, w: &str) -> pb::Explanation {
    for _ in 0..300 {
        let status = cp.status(w).await;
        if let Some(why) = status.unplaced.as_option().and_then(|u| u.explanation.as_option()) {
            return why.clone();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{w} was never refused: {:?}", cp.status(w).await);
}

#[tokio::test]
async fn a_device_is_given_to_the_workload_holding_it_and_to_another_once_it_is_freed() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "dev-1").await;
    standin(&node);
    let agent = node.agent().await;
    let repo = push("reader", reader()).await;
    let tenant = cp.token_for(&["workload.read", "workload.console.read"]).await;

    let first = cp.declare_body(&project, "first", &repo, asking()).await;
    node.guard(&first);
    converged(&cp, &first).await;
    assert_eq!(
        printed(&cp, &tenant, &first).await,
        ["read 4 given"],
        "the device, as its file gives it"
    );

    let second = cp.declare_body(&project, "second", &repo, asking()).await;
    node.guard(&second);
    let why = unplaced(&cp, &second).await;
    let device = why.filtered.iter().find(|f| f.check == "device");
    assert!(device.is_some_and(|f| f.detail.contains("0 of 1 serial")), "{why:?}");

    cp.retire(&first).await;
    converged(&cp, &second).await;
    assert_eq!(printed(&cp, &tenant, &second).await, ["read 4 given"], "freed, and given again");
    cp.retire(&second).await;
    agent.stop().await;
}
