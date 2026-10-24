// SPDX-License-Identifier: AGPL-3.0-or-later

//! A microVM's backup restored on a real node (§5.4 of docs/06-storage.md, §4.4 of
//! docs/04-runtimes.md): its volume is a block device, so a path of its backup is restored into a
//! new directory by a plan that stages the backup's image, stops the workload, has the
//! `firecracker` driver copy the path inside a guest, and starts it again, and the running guest
//! reads both what the backup held there and what it has written since; and a replacing restore
//! of the whole volume leaves a block device the workload starts on (Finding 159 of
//! docs/23-walkthroughs.md). Needs what the microVM test needs, and MinIO.

use crate::backups::{installed, nightly, serve, wire};
use crate::files::{Files, open};
use crate::microvm_files::{power, recipe};
use crate::microvm_install::read;
use crate::node::{Node, live};
use crate::recipes::push;
use crate::snapshots::{OPEN_VOLUME_PLANS, counted, plan_in};
use crate::support::{ControlPlane, converged};
use korpis_backup::testing::Minio;
use korpis_cli::cmd::backup::{BackupAction, backup};
use korpis_cli::connect::Connection;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request as File;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;

const WRITE: &str = "workload.file.write";

/// Writes `data` as the file `path` of a session `f`, its directory made.
async fn put(f: &mut Files, (dir, path): (&str, &str), data: &[u8]) {
    let mkdir = pb::FileMkdir { path: dir.into(), ..Default::default() };
    let _ = f.ask(File::Mkdir(Box::new(mkdir))).await;
    let written = f.write(path, 0, data.to_vec()).await;
    assert_eq!(written, Response::Written(u64::try_from(data.len()).unwrap()), "{path}");
}

/// Runs restore `r` as `operator`, approving its plan when it waits for a person, until no plan
/// of a volume is open.
async fn restored(cp: &ControlPlane, operator: &Connection, r: BackupAction) {
    let made = backup(operator, &r).await.unwrap();
    if made.human.contains("proposed") {
        cp.approve(&plan_in(&made.human)).await;
    }
    counted(cp, OPEN_VOLUME_PLANS, 0).await;
}

/// Backs up `w`'s volume into `repo` while it is stopped, so the image holds what it wrote, and
/// starts it again; the backup.
async fn taken(
    cp: &ControlPlane,
    operator: &Connection,
    (project, w): (&str, &str),
    repo: String,
) -> String {
    power(cp, w, false, 1).await;
    let data = cp.volume_named(project, "data").await;
    backup(operator, &BackupAction::Create { volume: data, repository: repo }).await.unwrap();
    counted(cp, "SELECT count(*) FROM backup WHERE status = 'succeeded'", 1).await;
    let taken: uuid::Uuid = cp.scalar("SELECT id FROM backup WHERE status = 'succeeded'").await;
    power(cp, w, true, 2).await;
    wire(IdKind::Backup, taken)
}

#[tokio::test]
async fn a_microvm_s_backup_restores_a_path_in_a_guest_and_replaces_its_volume_whole() {
    if !live() {
        return;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let scratch = tempfile::tempdir().unwrap();
    let Some(minio) = Minio::start(scratch.path()).await.unwrap() else { return };
    minio.bucket("backups").await.unwrap();
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let Some(node) = Node::join_with_microvms(&cp, "vm-restore").await else {
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
    put(&mut f, ("/data/world", "/data/world/level.dat"), b"seed 42").await;
    drop(f);
    let b = taken(&cp, &operator, (&project, &w), repo).await;
    let mut f = open(&editor, &w).await;
    put(&mut f, ("/data/world", "/data/world/level.dat"), b"lost 00").await;
    drop(f);

    let path = BackupAction::Restore {
        backup: b.clone(),
        into: Some("restored".into()),
        replace: false,
        volume: None,
        path: Some("world".into()),
    };
    restored(&cp, &operator, path).await;
    converged(&cp, &w).await;
    let mut f = open(&editor, &w).await;
    assert_eq!(read(&mut f, "/data/restored/world/level.dat").await, b"seed 42", "the backup's");
    assert_eq!(read(&mut f, "/data/world/level.dat").await, b"lost 00", "and the world's own");
    drop(f);

    let whole =
        BackupAction::Restore { backup: b, into: None, replace: true, volume: None, path: None };
    restored(&cp, &operator, whole).await;
    converged(&cp, &w).await;
    let mut f = open(&editor, &w).await;
    assert_eq!(read(&mut f, "/data/world/level.dat").await, b"seed 42", "replaced whole");
    let list = pb::FileList { path: "/data".into(), limit: 10, ..Default::default() };
    let Response::Entries(top) = f.ask(File::List(Box::new(list))).await else { panic!() };
    assert!(top.entries.iter().all(|e| e.name != "restored"), "as the backup held it");
    drop(f);
    cp.retire(&w).await;
    agent.stop().await;
}
