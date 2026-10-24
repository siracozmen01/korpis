// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block device's backup restored for reading through the helper's verbs (§5.4 of
//! docs/06-storage.md), with the real job binary and a real store, on ext4 with project quota: the
//! job restores the image into a place of its own beside the volume, marked a block device's, and
//! says where; the live volume is untouched; a commit is refused and leaves it; forgetting the job
//! discards it; one left when the volume is deleted goes with the volume; and a volume of files is
//! refused, its backup being read as it is. It needs root, the e2fsprogs, and MinIO
//! (`scripts/test-tools.sh`), and is skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

#[allow(dead_code, reason = "each test binary uses part of the shared fixture")]
mod fixture;
#[allow(dead_code, reason = "each test binary uses part of the shared fixture")]
#[path = "fixture/jobs.rs"]
mod jobs;

use jobs::{ask, finished, forget, job_binary, start, status};
use korpis_backup::testing::Minio;
use korpis_helper::block::{IMAGE, is_block};
use korpis_helper::helper::Helper;
use korpis_helper::ids::Ledger;
use korpis_helper::job::JobConfig;
use korpis_helper::storage::Class;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;
use std::path::Path;

const DISK: &str = "wl_1.disk";
const DATA: &str = "wl_1.data";

/// A helper running jobs over one class at `root`, holding a block device and a volume of files.
fn helper_over(root: &Path, state: &Path) -> Helper {
    let mut class = Class::open("reading", root).unwrap();
    for (volume, block_device) in [(DISK, true), (DATA, false)] {
        let r = helper::EnsureVolumeRequest {
            volume: volume.into(),
            workload: "wl_1".into(),
            size_bytes: 16 << 20,
            block_device,
            ..Default::default()
        };
        class.ensure(&r).unwrap();
    }
    let config = JobConfig {
        binary: job_binary(),
        install_binary: None,
        state: state.into(),
        cgroups: None,
        content: None,
    };
    Helper::new(vec![class], Ledger::open(state).unwrap()).with_jobs(config)
}

fn reading(volume: &str) -> Work {
    Work::Restore(Box::new(helper::RestoreVolume {
        volume: volume.into(),
        backup: "b1".into(),
        reading: true,
        ..Default::default()
    }))
}

/// Restores backup `b1` of the block device for reading as job `job`; the image it restored.
async fn read(h: &mut Helper, job: &str, endpoint: &str) -> std::path::PathBuf {
    assert!(
        matches!(ask(h, start(job, reading(DISK), endpoint)), Answer::StartJob(ref s) if s.started)
    );
    let result = finished(h, job).await;
    assert!(result.ok && result.restored.as_option().unwrap().verified, "{}", result.error);
    status(h, job).image.into()
}

#[tokio::test]
async fn a_block_device_s_backup_is_restored_beside_it_to_be_read_and_never_made_it() {
    if !rustix::process::geteuid().is_root() {
        return;
    }
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(fs) = fixture::ext4(tmp.path()) else { return };
    let Some(m) = Minio::start(tmp.path()).await.unwrap() else { return };
    m.bucket("backups").await.unwrap();
    let root = fs.0.join("volumes");
    let mut h = helper_over(&root, &tmp.path().join("state"));
    let live = root.join(DISK).join(IMAGE);
    let backed_up = std::fs::read(&live).unwrap();
    let backup = Work::Backup(Box::new(helper::BackupVolume {
        volume: DISK.into(),
        backup: "b1".into(),
        ..Default::default()
    }));
    ask(&mut h, start("bkp", backup, &m.endpoint));
    assert!(finished(&mut h, "bkp").await.ok);
    forget(&mut h, "bkp");
    let since = std::fs::OpenOptions::new().write(true).open(&live).unwrap();
    std::os::unix::fs::FileExt::write_at(&since, b"written since", 4096).unwrap();

    let image = read(&mut h, "read-1", &m.endpoint).await;
    assert_eq!(image, root.join(format!(".{DISK}.read+read-1")).join(IMAGE));
    assert_eq!(std::fs::read(&image).unwrap(), backed_up, "the backup's image, whole");
    assert!(is_block(image.parent().unwrap()), "marked, so a driver takes it");
    assert_ne!(std::fs::read(&live).unwrap(), backed_up, "the live volume untouched");
    let commit = helper::CommitJobRequest { job: "read-1".into(), ..Default::default() };
    assert!(matches!(ask(&mut h, Verb::CommitJob(Box::new(commit))), Answer::Error(_)));
    assert!(image.exists(), "a refused commit leaves it");
    assert!(matches!(forget(&mut h, "read-1"), Answer::ForgetJob(_)));
    assert!(!image.parent().unwrap().exists(), "forgotten, discarded");

    let files = ask(&mut h, start("read-2", reading(DATA), &m.endpoint));
    assert!(matches!(files, Answer::Error(_)), "a volume of files is read as it is: {files:?}");
    let image = read(&mut h, "read-3", &m.endpoint).await;
    let delete = helper::DeleteVolumeRequest { volume: DISK.into(), ..Default::default() };
    assert!(matches!(ask(&mut h, Verb::DeleteVolume(Box::new(delete))), Answer::DeleteVolume(_)));
    assert!(!image.parent().unwrap().exists(), "it holds the volume's data, and goes with it");
    let projects = std::fs::read_to_string(root.join(".projects")).unwrap();
    assert_eq!(projects.lines().count(), 1, "the volume of files' project alone: {projects}");
}
