// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backup and restore on a real node through the backup target for a disk (§5.6 of
//! docs/06-storage.md), the first-party extension run in a container on that node, serving its
//! own volume. Its node's jobs and the control plane's own reads reach it at its endpoint's
//! published address, over TLS that trusts its certificate alone: a running workload is backed
//! up twice, a prune forgets the first backup, a file is read from the second, and the second is
//! cloned into a new workload that runs on what it held. It needs root, containerd, and
//! `mkfs.btrfs`, and is skipped without them.

use crate::backups::{nightly, wire};
use crate::image::{FS_TARGET, push_image};
use crate::node::{Node, live};
use crate::recipes::{VERSION, push, push_sleeper, test_registry};
use crate::snapshots::{OPEN_VOLUME_PLANS, counted, lines, plan_in};
use crate::support::{ControlPlane, mutation};
use korpis_cli::cmd::backup::{BackupAction, backup};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

/// The target's document: its image, its volume, the endpoint it serves on, published on a port
/// of its node, and the backup target it provides.
fn document(image: &str) -> String {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Extension
metadata: {{name: fs-target, version: "{VERSION}", description: backups on a disk, license: AGPL-3.0-or-later}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{image}"}}
  entry:
    command: [{entry}]
preset:
  lifecycle: persistent
  console: none
  volumes:
    - {{name: objects, path: /data, size_default: 64Mi}}
  endpoints:
    - {{name: store, protocol: tcp, port: 9000, exposure_default: node_port}}
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu: {{min: 100m, recommended: 250m}}
extension:
  api_version: korpis.extension.v1
  requires: [provider.serve, provider.backup_target]
  provides:
    - {{type: backup_target, name: fs}}
"#,
        entry = FS_TARGET.entry(),
    )
}

/// Installs the target into `project`; its workload.
async fn installed(cp: &ControlPlane, project: &str) -> String {
    let image = push_image(&test_registry(), FS_TARGET).await;
    let request = pb::InstallExtensionRequest {
        mutation: mutation("install-fs").into(),
        artifact: push("fs-target", document(&image)).await,
        version: VERSION.into(),
        organization: cp.root_org.clone(),
        workload_project: project.to_owned(),
        ..Default::default()
    };
    let done = pb::ExtensionServiceClient::new(cp.http(), cp.config())
        .install_extension(request)
        .await
        .unwrap()
        .into_owned();
    done.extension.into_option().unwrap().workload
}

/// Waits until the target answers a ping.
async fn registered(cp: &ControlPlane) {
    let client = pb::ProviderServiceClient::new(cp.http(), cp.config());
    let ping = pb::PingProviderRequest {
        organization: cp.root_org.clone(),
        r#type: pb::ProviderType::PROVIDER_TYPE_BACKUP_TARGET.into(),
        name: "fs".into(),
        ..Default::default()
    };
    for _ in 0..240 {
        let answered = client.ping_provider(ping.clone()).await;
        if answered.is_ok_and(|p| p.into_owned().error.as_option().is_none()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("the target never registered");
}

#[tokio::test]
async fn a_running_world_is_backed_up_to_a_disk_read_cloned_and_pruned() {
    if !live() {
        return;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let Some(node) = Node::join_on_btrfs(&cp, "disk-1").await else { return };
    let agent = node.agent().await;
    let target = installed(&cp, &project).await;
    node.guard(&target);
    registered(&cp).await;

    let operator = cp.cli(None);
    let repo = nightly(&operator, "fs").await;
    let w = cp.declare(&project, "world", &push_sleeper().await).await;
    node.guard(&w);
    let vol = node.volumes.join(format!("{w}.data"));
    lines(&vol.join("starts"), 1).await;
    std::fs::create_dir_all(vol.join("saves")).unwrap();
    std::fs::write(vol.join("saves/level.dat"), "the first world").unwrap();
    let data = cp.volume_named(&project, "data").await;
    let first = BackupAction::Create { volume: data.clone(), repository: repo.clone() };
    backup(&operator, &first).await.unwrap();
    counted(&cp, "SELECT count(*) FROM backup WHERE status = 'succeeded'", 1).await;
    // The backup is recorded before its plan closes, and a volume takes one plan at a time.
    counted(&cp, OPEN_VOLUME_PLANS, 0).await;
    let disk = node.volumes.join(format!("{target}.objects")).join("objects/korpis");
    assert!(disk.exists(), "the backup is on the target's own volume");

    std::fs::write(vol.join("saves/level.dat"), "the second world").unwrap();
    backup(&operator, &BackupAction::Create { volume: data, repository: repo.clone() })
        .await
        .unwrap();
    counted(&cp, "SELECT count(*) FROM backup WHERE status = 'succeeded'", 2).await;
    let prune = BackupAction::Prune { workload: w.clone(), repository: repo };
    cp.approve(&plan_in(&backup(&operator, &prune).await.unwrap().human)).await;
    counted(&cp, "SELECT count(*) FROM backup WHERE deleted_at IS NOT NULL", 1).await;
    let kept: uuid::Uuid = cp.scalar("SELECT id FROM backup WHERE deleted_at IS NULL").await;
    let b2 = wire(IdKind::Backup, kept);

    // Read by the control plane from the target, at its published address.
    let mut read = Vec::new();
    korpis_cli::cmd::backup::cat(&operator, (&b2, "saves/level.dat"), &mut read).await.unwrap();
    assert_eq!(read, b"the second world");

    // Restored by the node from the target, into a new workload.
    let copying = korpis_cli::cmd::clone::clone(&operator, &b2, ("world-copy", None));
    Box::pin(copying).await.unwrap();
    let copy: uuid::Uuid = cp.scalar("SELECT id FROM workload WHERE name = 'world-copy'").await;
    let copy = wire(IdKind::Workload, copy);
    node.guard(&copy);
    let copied = node.volumes.join(format!("{copy}.data"));
    lines(&copied.join("starts"), 2).await;
    let level = std::fs::read_to_string(copied.join("saves/level.dat")).unwrap();
    assert_eq!(level, "the second world");
    agent.stop().await;
}
