// SPDX-License-Identifier: AGPL-3.0-or-later

//! An extension written with the SDK, run in a container on a real node (§3 of
//! docs/16-extensions.md): installed, it registers its provider and is pinged through the API,
//! and it follows its installation's subscription, printing what it hears to its console. The
//! control plane listens on loopback, which no workload's namespace reaches, so everything it
//! does crosses the relay its node serves it (§9.5 of docs/08-identity.md).

use crate::image::{TEST_EXTENSION, push_image};
use crate::node::{Node, live};
use crate::recipes::{VERSION, push};
use crate::support::{ControlPlane, mutation};
use korpis_proto::korpis::v1 as pb;
use korpis_stream::dir::StreamDir;
use korpis_stream::read::read;
use std::time::Duration;

/// The extension's document: the image, a DNS provider named `test`, and a subscription to
/// project events.
fn document(image: &str) -> String {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Extension
metadata: {{name: test-extension, version: "{VERSION}", description: tests, license: Apache-2.0}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{image}"}}
  entry:
    command: [{entry}]
preset:
  lifecycle: persistent
  console: logs
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu: {{min: 50m, recommended: 100m}}
extension:
  api_version: korpis.extension.v1
  requires: [provider.serve, provider.dns, events.subscribe]
  needs:
    - {{actions: [event.read], scope: organization, reason: to hear of projects}}
  subscribes:
    - {{kinds: [project.], min_severity: info}}
  provides:
    - {{type: dns, name: test}}
"#,
        entry = TEST_EXTENSION.entry(),
    )
}

/// Installs the extension at `repo` into `project`, approving what it needs; the installation.
pub async fn install(cp: &ControlPlane, repo: &str, project: &str) -> pb::Extension {
    let client = pb::ExtensionServiceClient::new(cp.http(), cp.config());
    let request = |name: &str| pb::InstallExtensionRequest {
        mutation: mutation(name).into(),
        artifact: repo.to_owned(),
        version: VERSION.into(),
        organization: cp.root_org.clone(),
        workload_project: project.to_owned(),
        ..Default::default()
    };
    let dry = pb::InstallExtensionRequest { dry_run: true, ..request("dry") };
    let shown = client.install_extension(dry).await.unwrap().into_owned();
    let needs = shown.extension.into_option().unwrap().manifest.into_option().unwrap().needs;
    let approved = pb::InstallExtensionRequest { approved_needs: needs, ..request("install") };
    let done = client.install_extension(approved).await.unwrap().into_owned();
    done.extension.into_option().unwrap()
}

/// Pings the provider of `kind` named `name` until it answers, for up to two minutes; its
/// version.
pub async fn pinged_as(cp: &ControlPlane, kind: pb::ProviderType, name: &str) -> String {
    let client = pb::ProviderServiceClient::new(cp.http(), cp.config());
    let ping = pb::PingProviderRequest {
        organization: cp.root_org.clone(),
        r#type: kind.into(),
        name: name.into(),
        ..Default::default()
    };
    let mut last = String::new();
    for _ in 0..240 {
        match client.ping_provider(ping.clone()).await.map(connectrpc::UnaryResponse::into_owned) {
            Ok(p) if p.error.as_option().is_none() => return p.version,
            Ok(p) => last = format!("{:?}", p.error),
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("the extension never answered a ping: {last}");
}

/// Pings the test extension's DNS provider until it answers; its version.
async fn pinged(cp: &ControlPlane) -> String {
    pinged_as(cp, pb::ProviderType::PROVIDER_TYPE_DNS, "test").await
}

/// Waits for the extension's console to say `line`.
async fn printed(node: &Node, workload: &str, line: &str) {
    let console = node.volumes.join(format!("{workload}.stream_console"));
    let mut said = Vec::new();
    for _ in 0..300 {
        if let Ok(r) = read(&StreamDir::new(&console), 0, 1000) {
            said =
                r.records.iter().map(|r| String::from_utf8_lossy(&r.body).into_owned()).collect();
            if said.concat().lines().any(|l| l.starts_with(line)) {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("its console never said {line:?}: {said:?}");
}

#[tokio::test]
async fn an_extension_in_a_container_serves_and_consumes_through_its_node() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "ext-1").await;
    let agent = node.agent().await;
    let registry = crate::recipes::test_registry();
    let image = push_image(&registry, TEST_EXTENSION).await;
    let repo = push("test-extension", document(&image)).await;
    let workload = install(&cp, &repo, &project).await.workload;
    node.guard(&workload);

    assert_eq!(pinged(&cp).await, env!("CARGO_PKG_VERSION"), "registered and answering");
    let relay = node.state.root().join("identity").join(&workload).join("api.sock");
    assert!(relay.exists(), "its node serves it the relay");
    printed(&node, &workload, "following 1").await;
    let heard = pb::CreateProjectRequest {
        mutation: mutation("heard").into(),
        organization: cp.root_org.clone(),
        name: "heard".into(),
        ..Default::default()
    };
    pb::ProjectServiceClient::new(cp.http(), cp.config()).create_project(heard).await.unwrap();
    printed(&node, &workload, "event project.create").await;
    agent.stop().await;
}
