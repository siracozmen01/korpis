// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backup and restore on a real node (§5 of docs/06-storage.md), its volumes on a btrfs made for
//! the test, its repository in a real object store (MinIO), reached through the first-party S3
//! target provider: an extension the node runs, whose provider the test serves with the
//! credential the node hands the extension's workload through its identity socket, as its
//! container would. A running workload is backed up twice by its node, and a prune approved
//! forgets the first, which no one read; the second is listed, one file read from it, and cloned
//! into a new workload that runs on what it held. It needs root, containerd, `mkfs.btrfs`, and
//! MinIO, and is skipped without them.

use crate::node::{Node, live};
use crate::recipes::{VERSION, push_sleeper, push_target};
use crate::snapshots::{OPEN_VOLUME_PLANS, counted, lines, plan_in};
use crate::support::{ControlPlane, mutation};
use korpis_backup::testing::{Minio, SECRET, USER};
use korpis_cli::cmd::backup::{BackupAction, backup};
use korpis_cli::cmd::repository::{RepositoryAction, repository};
use korpis_core::id::IdKind;
use korpis_extension::backup::Backup;
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, IdentitySocket};
use korpis_proto::korpis::v1 as pb;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The first-party S3 target over a MinIO bucket.
fn real(minio: &Minio) -> korpis_target_s3::S3Target {
    let endpoint: url::Url = minio.endpoint.parse().unwrap();
    let sts = korpis_target_s3::sts::Sts {
        endpoint: endpoint.clone(),
        region: "us-east-1".into(),
        access_key_id: USER.into(),
        secret: SECRET.into(),
        role_arn: None,
        http: reqwest::Client::new(),
    };
    let place = korpis_target_s3::Place {
        endpoint,
        region: "us-east-1".into(),
        bucket: "backups".into(),
        root: String::new(),
        path_style: true,
        access_key_id: USER.into(),
        secret: SECRET.into(),
    };
    korpis_target_s3::S3Target::new(place, sts).unwrap()
}

/// Installs the target extension into `project`; its workload.
async fn installed(cp: &ControlPlane, project: &str) -> String {
    let request = pb::InstallExtensionRequest {
        mutation: mutation("install-target").into(),
        artifact: push_target().await,
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

/// Waits for `path` to exist.
async fn exists(path: &Path) {
    for _ in 0..300 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{} never appeared", path.display());
}

/// Serves the backup target as the extension's workload, from its identity socket at `socket`.
async fn serve(socket: PathBuf, minio: &Minio) {
    exists(&socket).await;
    let connection = Arc::new(Connection::new(IdentitySocket::at(socket)));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_BACKUP_TARGET,
        name: "s3".into(),
        version: VERSION.into(),
        concurrency: 4,
        sign_in: false,
    };
    let provider = Arc::new(Provider::new(connection, registration, Backup(real(minio))));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    registered.wait_for(Option::is_some).await.unwrap();
}

/// A repository through the target `provider` keeping one backup of each volume; its identifier.
pub async fn nightly(operator: &korpis_cli::connect::Connection, provider: &str) -> String {
    let made = RepositoryAction::Create {
        name: "nightly".into(),
        provider: provider.into(),
        prefix: "korpis/".into(),
        organization: None,
        project: None,
        keep_last: 1,
        keep_daily: 0,
        keep_weekly: 0,
        keep_monthly: 0,
        min_retention_days: 0,
        streams: false,
        no_streams: false,
        kms_provider: None,
        kms_key: None,
    };
    repository(operator, &made).await.unwrap().json["id"].as_str().unwrap().to_owned()
}

/// Every backup recorded, with its status and when it was forgotten, for a failure to show.
const EVERY_BACKUP: &str = "SELECT string_agg(id || ' ' || status || ' ' || \
                            coalesce(deleted_at::text, 'kept'), ', ' ORDER BY id) FROM backup";

/// A uuid column's value as the API names it.
pub fn wire(kind: IdKind, id: uuid::Uuid) -> String {
    korpis_wire::wire_id(kind, id)
}

#[tokio::test]
async fn a_running_world_is_backed_up_read_cloned_and_pruned() {
    if !live() {
        return;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let scratch = tempfile::tempdir().unwrap();
    let Some(minio) = Minio::start(scratch.path()).await.unwrap() else { return };
    minio.bucket("backups").await.unwrap();
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let Some(node) = Node::join_on_btrfs(&cp, "fra-5").await else { return };
    let agent = node.agent().await;
    let extension = installed(&cp, &project).await;
    node.guard(&extension);
    let socket = node.state.root().join("identity").join(&extension).join("identity.sock");
    serve(socket, &minio).await;

    let operator = cp.cli(None);
    let repo = nightly(&operator, "s3").await;

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

    // A second backup, and a prune keeping the last one forgets the first, which no one reads.
    std::fs::write(vol.join("saves/level.dat"), "the second world").unwrap();
    backup(&operator, &BackupAction::Create { volume: data, repository: repo.clone() })
        .await
        .unwrap();
    counted(&cp, "SELECT count(*) FROM backup WHERE status = 'succeeded'", 2).await;
    let prune = BackupAction::Prune { workload: w.clone(), repository: repo };
    cp.approve(&plan_in(&backup(&operator, &prune).await.unwrap().human)).await;
    counted(&cp, "SELECT count(*) FROM backup WHERE deleted_at IS NOT NULL", 1).await;
    // Of the two that succeeded: a backup that failed on a loaded machine is kept a while too.
    let kept: uuid::Uuid =
        cp.scalar("SELECT id FROM backup WHERE deleted_at IS NULL AND status = 'succeeded'").await;
    let forgotten: uuid::Uuid =
        cp.scalar("SELECT id FROM backup WHERE deleted_at IS NOT NULL").await;
    let every: String = cp.scalar(EVERY_BACKUP).await;
    assert!(forgotten < kept, "the older one is forgotten: {every}");
    let b2 = wire(IdKind::Backup, kept);

    // One directory listed, one file read: from the repository, not from the node.
    let ls = BackupAction::Ls { backup: b2.clone(), path: "saves".into() };
    assert!(backup(&operator, &ls).await.unwrap().human.contains("level.dat"));
    let mut read = Vec::new();
    korpis_cli::cmd::backup::cat(&operator, (&b2, "saves/level.dat"), &mut read).await.unwrap();
    assert_eq!(read, b"the second world");

    // Cloned: a new workload, running on what the backup held.
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
