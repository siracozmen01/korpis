// SPDX-License-Identifier: AGPL-3.0-or-later

//! A replacing restore through the helper's verbs, with the real job binary and a real store, on
//! a plain directory class, on btrfs, and on ext4 with project quota: the job restores into a staging place while the volume
//! is untouched; committing makes the volume what the backup held, with its limit kept, and doing
//! it again changes nothing; one forgotten without a commit is discarded and the volume left as
//! it was. It needs root and MinIO (`scripts/test-tools.sh`), and the filesystem it names.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

#[allow(dead_code, reason = "each test binary uses part of the shared fixture")]
mod fixture;
#[path = "fixture/jobs.rs"]
mod jobs;

use jobs::{ask, finished, forget, job_binary, start};
use korpis_backup::testing::Minio;
use korpis_helper::helper::Helper;
use korpis_helper::ids::Ledger;
use korpis_helper::job::JobConfig;
use korpis_helper::storage::Class;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;
use std::path::Path;

const VOLUME: &str = "wl_1.data";

/// A helper running jobs over one class at `root`, with the volume holding a world.
fn helper_over(root: &Path, state: &Path) -> Helper {
    let mut class = Class::open("restore", root).unwrap();
    let r = helper::EnsureVolumeRequest {
        volume: VOLUME.into(),
        workload: "wl_1".into(),
        size_bytes: 1 << 30,
        ..Default::default()
    };
    class.ensure(&r).unwrap();
    std::fs::create_dir(class.path_of(VOLUME).join("world")).unwrap();
    std::fs::write(class.path_of(VOLUME).join("world/level.dat"), b"the world").unwrap();
    let config =
        JobConfig { binary: job_binary(), state: state.into(), cgroups: None, content: None };
    Helper::new(vec![class], Ledger::open(state).unwrap()).with_jobs(config)
}

fn commit(h: &mut Helper, job: &str) -> Answer {
    ask(
        h,
        Verb::CommitJob(Box::new(helper::CommitJobRequest {
            job: job.into(),
            ..Default::default()
        })),
    )
}

fn limit(h: &mut Helper) -> u64 {
    let r = helper::VolumeUsageRequest { volume: VOLUME.into(), ..Default::default() };
    match ask(h, Verb::VolumeUsage(Box::new(r))) {
        Answer::VolumeUsage(u) => u.limit_bytes,
        other => panic!("{other:?}"),
    }
}

/// Restores backup `b1` of the volume over it as job `job`, until the job has finished.
async fn staged(h: &mut Helper, job: &str, endpoint: &str) {
    let work = Work::Restore(Box::new(helper::RestoreVolume {
        volume: VOLUME.into(),
        backup: "b1".into(),
        replace: true,
        ..Default::default()
    }));
    assert!(
        matches!(ask(h, start(job, work.clone(), endpoint)), Answer::StartJob(ref s) if s.started)
    );
    let result = finished(h, job).await;
    assert!(result.ok && result.restored.as_option().unwrap().verified, "{}", result.error);
    assert!(
        matches!(ask(h, start(job, work, endpoint)), Answer::StartJob(ref s) if !s.started),
        "started once, and its staging place not made again"
    );
}

/// The whole story over the class at `root`.
async fn replaces(root: &Path, state: &Path, endpoint: &str) {
    let mut h = helper_over(root, state);
    let backup = Work::Backup(Box::new(helper::BackupVolume {
        volume: VOLUME.into(),
        backup: "b1".into(),
        ..Default::default()
    }));
    ask(&mut h, start("bkp", backup, endpoint));
    assert!(finished(&mut h, "bkp").await.ok);
    forget(&mut h, "bkp");
    let (volume, staging) = (root.join(VOLUME), root.join(format!(".{VOLUME}.restore")));
    let before = limit(&mut h);

    std::fs::write(volume.join("since.txt"), b"written after the backup").unwrap();
    staged(&mut h, "rst-1", endpoint).await;
    assert!(volume.join("since.txt").exists(), "the volume is untouched until the commit");
    assert!(staging.join("world/level.dat").exists());
    assert!(matches!(commit(&mut h, "rst-1"), Answer::CommitJob(_)));
    assert!(!volume.join("since.txt").exists(), "the volume is what the backup held");
    assert_eq!(std::fs::read(volume.join("world/level.dat")).unwrap(), b"the world");
    assert!(!staging.exists(), "what was the volume is gone");
    assert_eq!(limit(&mut h), before, "the volume keeps its limit");
    assert!(matches!(commit(&mut h, "rst-1"), Answer::CommitJob(_)), "again, nothing changes");
    assert!(volume.join("world/level.dat").exists());
    assert!(matches!(forget(&mut h, "rst-1"), Answer::ForgetJob(_)));
    assert!(volume.join("world/level.dat").exists(), "forgetting a committed restore keeps it");

    std::fs::write(volume.join("kept.txt"), b"not replaced").unwrap();
    staged(&mut h, "rst-2", endpoint).await;
    assert!(matches!(forget(&mut h, "rst-2"), Answer::ForgetJob(_)));
    assert!(
        volume.join("kept.txt").exists() && !staging.exists(),
        "discarded, the volume as it was"
    );
    assert!(matches!(commit(&mut h, "rst-2"), Answer::Error(_)), "a forgotten job commits nothing");
}

#[tokio::test]
async fn a_replacing_restore_on_a_directory_class_is_committed_or_discarded() {
    if !rustix::process::geteuid().is_root() {
        return;
    }
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(m) = Minio::start(tmp.path()).await.unwrap() else { return };
    m.bucket("backups").await.unwrap();
    replaces(&tmp.path().join("volumes"), &tmp.path().join("state"), &m.endpoint).await;
}

#[tokio::test]
async fn a_replacing_restore_on_btrfs_keeps_the_volumes_limit() {
    if !rustix::process::geteuid().is_root() {
        return;
    }
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(fs) = fixture::mounted(tmp.path(), true) else { return };
    let Some(m) = Minio::start(tmp.path()).await.unwrap() else { return };
    m.bucket("backups").await.unwrap();
    replaces(&fs.0.join("volumes"), &tmp.path().join("state"), &m.endpoint).await;
}

#[tokio::test]
async fn a_replacing_restore_with_project_quota_moves_the_volume_to_its_project() {
    if !rustix::process::geteuid().is_root() {
        return;
    }
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(fs) = fixture::ext4(tmp.path()) else { return };
    let Some(m) = Minio::start(tmp.path()).await.unwrap() else { return };
    m.bucket("backups").await.unwrap();
    replaces(&fs.0.join("volumes"), &tmp.path().join("state"), &m.endpoint).await;
    let projects = std::fs::read_to_string(fs.0.join("volumes/.projects")).unwrap();
    assert_eq!(projects.lines().count(), 1, "one project, the volume's: {projects}");
}
