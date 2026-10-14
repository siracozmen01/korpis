// SPDX-License-Identifier: AGPL-3.0-or-later

//! Jobs through the helper's verbs, with the real job binary and a real store: a volume backed up
//! live, recorded as such; one path of it restored into a directory the helper makes in another
//! volume, owned as that volume is; a job started twice runs once; one stuck on a store that
//! never answers is cancelled and reported lost; and what the helper kept is forgotten. It needs
//! root and MinIO (`scripts/test-tools.sh`), and is skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

use buffa::Message as _;
use korpis_backup::testing::Minio;
use korpis_helper::helper::Helper;
use korpis_helper::ids::Ledger;
use korpis_helper::job::JobConfig;
use korpis_helper::storage::Class;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;
use korpis_proto::korpis::v1 as api;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The job binary, built where this test was.
fn job_binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(Path::parent).unwrap();
    let profile = match profile_dir.file_name().unwrap().to_str().unwrap() {
        "debug" => "dev",
        other => other,
    };
    let built = std::process::Command::new(env!("CARGO"))
        .args([
            "build",
            "-q",
            "-p",
            "korpis-backup",
            "--bin",
            "korpis-backup-job",
            "--profile",
            profile,
            "--target-dir",
        ])
        .arg(profile_dir.parent().unwrap())
        .status()
        .unwrap();
    assert!(built.success());
    profile_dir.join("korpis-backup-job")
}

fn ask(h: &mut Helper, verb: Verb) -> Answer {
    let request = helper::HelperRequest { verb: Some(verb), ..Default::default() };
    helper::HelperResponse::decode_from_slice(&h.handle(&request.encode_to_vec()))
        .unwrap()
        .result
        .unwrap()
}

fn store(endpoint: &str) -> api::S3Access {
    api::S3Access {
        endpoint: endpoint.into(),
        region: "us-east-1".into(),
        bucket: "backups".into(),
        key_prefix: "repo/wl_1/".into(),
        access_key_id: korpis_backup::testing::USER.into(),
        secret_access_key: korpis_backup::testing::SECRET.into(),
        path_style: true,
        ..Default::default()
    }
}

fn start(job: &str, work: Work, endpoint: &str) -> Verb {
    Verb::StartJob(Box::new(helper::StartJobRequest {
        job: job.into(),
        work: Some(work),
        data_key: vec![1; 32],
        store: store(endpoint).into(),
        concurrency: 4,
        ..Default::default()
    }))
}

fn status(h: &mut Helper, job: &str) -> helper::JobStatusResponse {
    match ask(
        h,
        Verb::JobStatus(Box::new(helper::JobStatusRequest {
            job: job.into(),
            ..Default::default()
        })),
    ) {
        Answer::JobStatus(s) => *s,
        other => panic!("{other:?}"),
    }
}

async fn finished(h: &mut Helper, job: &str) -> korpis_proto::korpis::backup::v1::JobResult {
    for _ in 0..600 {
        let s = status(h, job);
        if s.state.as_known() == Some(helper::JobState::JOB_STATE_FINISHED) {
            return s.result.into_option().unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {job} did not finish");
}

/// A helper running jobs, with volumes `wl_1.data` (holding a world) and `wl_2.data` (owned by
/// 1234), and the cgroup its jobs' cgroups are made in.
fn setup(tmp: &Path) -> (Helper, PathBuf) {
    let mut class = Class::open("local", &tmp.join("volumes")).unwrap();
    for v in ["wl_1.data", "wl_2.data"] {
        let r = helper::EnsureVolumeRequest {
            volume: v.into(),
            workload: v.split('.').next().unwrap().into(),
            size_bytes: 1 << 30,
            ..Default::default()
        };
        class.ensure(&r).unwrap();
    }
    let data = class.path_of("wl_1.data");
    std::fs::create_dir(data.join("world")).unwrap();
    std::fs::write(data.join("world/level.dat"), vec![4u8; 1 << 20]).unwrap();
    std::os::unix::fs::chown(class.path_of("wl_2.data"), Some(1234), Some(1234)).unwrap();
    let cgroups = PathBuf::from(format!("/sys/fs/cgroup/korpis-test-{}", std::process::id()));
    let config = JobConfig {
        binary: job_binary(),
        state: tmp.join("state"),
        cgroups: Some(cgroups.clone()),
        content: None,
    };
    let h = Helper::new(vec![class], Ledger::open(&tmp.join("state")).unwrap()).with_jobs(config);
    (h, cgroups)
}

/// One path of backup `b1` restored into a directory made in `wl_2.data`, and refused again.
async fn restored(h: &mut Helper, tmp: &Path, endpoint: &str) {
    let restore = Work::Restore(Box::new(helper::RestoreVolume {
        volume: "wl_2.data".into(),
        backup: "b1".into(),
        path: "world".into(),
        into: "restored".into(),
        ..Default::default()
    }));
    assert!(matches!(ask(h, start("job-r", restore.clone(), endpoint)), Answer::StartJob(_)));
    let result = finished(h, "job-r").await;
    assert!(result.ok && result.restored.as_option().unwrap().verified, "{}", result.error);
    let into = tmp.join("volumes/wl_2.data/restored");
    assert_eq!(std::fs::metadata(&into).unwrap().uid(), 1234, "the workload reaches it");
    assert_eq!(std::fs::read(into.join("world/level.dat")).unwrap().len(), 1 << 20);
    let again = ask(h, start("job-r2", restore, endpoint));
    assert!(
        matches!(again, Answer::Error(e) if e.message.contains("already exists")),
        "never over what is there"
    );
}

#[tokio::test]
async fn jobs_run_confined_through_the_helper_and_are_forgotten() {
    if !rustix::process::geteuid().is_root() {
        eprintln!("jobs need root: not run");
        return;
    }
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(m) = Minio::start(tmp.path()).await.unwrap() else { return };
    m.bucket("backups").await.unwrap();
    let (mut h, cgroups) = setup(tmp.path());

    let backup = Work::Backup(Box::new(helper::BackupVolume {
        volume: "wl_1.data".into(),
        backup: "b1".into(),
        ..Default::default()
    }));
    assert!(
        matches!(ask(&mut h, start("job-b", backup.clone(), &m.endpoint)), Answer::StartJob(s) if s.started)
    );
    assert!(
        matches!(ask(&mut h, start("job-b", backup, &m.endpoint)), Answer::StartJob(s) if !s.started),
        "once"
    );
    let result = finished(&mut h, "job-b").await;
    assert!(result.ok, "{}", result.error);
    assert!(!result.backup.as_option().unwrap().consistent, "a live read says so");
    assert!(result.confinement.as_option().unwrap().cgroup);

    restored(&mut h, tmp.path(), &m.endpoint).await;

    for job in ["job-b", "job-r"] {
        assert!(matches!(
            ask(
                &mut h,
                Verb::ForgetJob(Box::new(helper::ForgetJobRequest {
                    job: job.into(),
                    ..Default::default()
                }))
            ),
            Answer::ForgetJob(_)
        ));
        assert_eq!(status(&mut h, job).state.as_known(), Some(helper::JobState::JOB_STATE_UNKNOWN));
    }
    assert!(!cgroups.join("job-job-b").exists());
    let _ = std::fs::remove_dir(&cgroups);
}

#[tokio::test]
async fn a_job_stuck_on_its_store_is_cancelled_and_lost() {
    if !rustix::process::geteuid().is_root() {
        return;
    }
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let mut class = Class::open("local", &tmp.path().join("volumes")).unwrap();
    let r = helper::EnsureVolumeRequest {
        volume: "wl_1.data".into(),
        workload: "wl_1".into(),
        ..Default::default()
    };
    class.ensure(&r).unwrap();
    // A store that accepts and never answers.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", silent.local_addr().unwrap());
    let config = JobConfig {
        binary: job_binary(),
        state: tmp.path().join("state"),
        cgroups: None,
        content: None,
    };
    let mut h = Helper::new(vec![class], Ledger::open(&tmp.path().join("state")).unwrap())
        .with_jobs(config);
    let work = Work::Backup(Box::new(helper::BackupVolume {
        volume: "wl_1.data".into(),
        backup: "b1".into(),
        ..Default::default()
    }));
    ask(&mut h, start("stuck", work, &endpoint));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(status(&mut h, "stuck").state.as_known(), Some(helper::JobState::JOB_STATE_RUNNING));
    assert!(h.busy(), "it does not exit while a job runs");
    let cancel = ask(
        &mut h,
        Verb::CancelJob(Box::new(helper::CancelJobRequest {
            job: "stuck".into(),
            ..Default::default()
        })),
    );
    assert!(matches!(cancel, Answer::CancelJob(c) if c.cancelled));
    assert_eq!(status(&mut h, "stuck").state.as_known(), Some(helper::JobState::JOB_STATE_LOST));
    let refused = ask(
        &mut h,
        Verb::JobStatus(Box::new(helper::JobStatusRequest {
            job: "../x".into(),
            ..Default::default()
        })),
    );
    assert!(matches!(refused, Answer::Error(_)));
}
