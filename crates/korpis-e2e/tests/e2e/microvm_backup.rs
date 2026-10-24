// SPDX-License-Identifier: AGPL-3.0-or-later

//! A microVM's backup read inside its image on a real node (§5.4 of docs/06-storage.md, §4.4 of
//! docs/04-runtimes.md): its volume is a block device, so its backup is one `ext4` image, which
//! `korpis backup list` says; `korpis backup ls` and `cat` open a session through the relay, the
//! node restores the image beside the volume and the `firecracker` driver serves it from a guest
//! of its own while the workload's own guest runs, and what the guest wrote before the backup is
//! listed and read back; `--image` lists the backup as the repository holds it; and the copy is
//! gone once the sessions end. Needs what the microVM test needs, and MinIO.

use crate::backups::{installed, nightly, serve, wire};
use crate::files::open;
use crate::microvm_files::{power, recipe};
use crate::node::{Node, live};
use crate::recipes::push;
use crate::snapshots::counted;
use crate::support::{ControlPlane, converged};
use korpis_backup::testing::Minio;
use korpis_cli::cmd::backup::{BackupAction, backup, inside};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use std::time::Duration;

const WRITE: &str = "workload.file.write";

#[tokio::test]
async fn a_microvm_s_backup_is_listed_and_read_inside_its_image_in_a_guest() {
    if !live() {
        return;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let scratch = tempfile::tempdir().unwrap();
    let Some(minio) = Minio::start(scratch.path()).await.unwrap() else { return };
    minio.bucket("backups").await.unwrap();
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let Some(node) = Node::join_with_microvms(&cp, "vm-backup").await else {
        eprintln!("this machine cannot run a guest: not run");
        return;
    };
    let agent = node.agent().await;
    let extension = installed(&cp, &project).await;
    node.guard(&extension);
    let socket = node.state.root().join("identity").join(&extension).join("identity.sock");
    serve(socket, &minio).await;
    let operator = cp.cli(None);
    let repo = nightly(&operator, "s3").await;

    let w = cp.declare(&project, "guest", &push("guest-files", recipe()).await).await;
    node.guard(&w);
    converged(&cp, &w).await;
    let editor = cp.cli(Some(&cp.token_for(&[WRITE]).await));
    let mut f = open(&editor, &w).await;
    assert_eq!(f.write("/data/level.dat", 0, b"seed 42".to_vec()).await, Response::Written(7));
    drop(f);
    // Stopped, so its volume is synced and the image holds what it wrote.
    power(&cp, &w, false, 1).await;
    let data = cp.volume_named(&project, "data").await;
    backup(&operator, &BackupAction::Create { volume: data, repository: repo }).await.unwrap();
    counted(&cp, "SELECT count(*) FROM backup WHERE status = 'succeeded'", 1).await;
    let taken: uuid::Uuid = cp.scalar("SELECT id FROM backup WHERE status = 'succeeded'").await;
    let b = wire(IdKind::Backup, taken);
    power(&cp, &w, true, 2).await;

    let list = BackupAction::List { workload: w.clone(), deleted: false };
    assert!(backup(&operator, &list).await.unwrap().human.contains("ext4"), "an ext4 image");
    let ls = BackupAction::Ls { backup: b.clone(), path: String::new(), image: false };
    let listed = backup(&operator, &ls).await.unwrap().human;
    assert!(listed.contains("level.dat"), "inside the image: {listed}");
    let mut read = Vec::new();
    inside::cat(&operator, (&w, &b), "level.dat", &mut read).await.unwrap();
    assert_eq!(read, b"seed 42", "read in a guest beside the running workload's");
    let image = BackupAction::Ls { backup: b.clone(), path: String::new(), image: true };
    let whole = backup(&operator, &image).await.unwrap().human;
    assert!(whole.contains("block.ext4"), "as the repository holds it: {whole}");

    let copies = || {
        let dir = std::fs::read_dir(&node.volumes).unwrap();
        let reading = format!(".{w}.data.read+");
        dir.flatten().any(|e| e.file_name().to_string_lossy().starts_with(&reading))
    };
    for _ in 0..100 {
        if !copies() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(!copies(), "the copy is discarded once the sessions end");
    cp.retire(&w).await;
    agent.stop().await;
}
