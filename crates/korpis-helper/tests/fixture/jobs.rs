// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the helper's job tests share: the job binary built where they were, and asking a helper
//! its job verbs.

use buffa::Message as _;
use korpis_helper::helper::Helper;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;
use korpis_proto::korpis::v1 as api;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The job binary, built where this test was.
pub fn job_binary() -> PathBuf {
    built("korpis-backup", "korpis-backup-job")
}

/// The install job binary, built where this test was.
pub fn install_binary() -> PathBuf {
    built("korpis-install", "korpis-install-job")
}

/// The file session worker binary, built where this test was.
pub fn files_binary() -> PathBuf {
    built("korpis-files", "korpis-fsworker")
}

/// The binary `bin` of `package`, built where this test was.
fn built(package: &str, bin: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(Path::parent).unwrap();
    let profile = match profile_dir.file_name().unwrap().to_str().unwrap() {
        "debug" => "dev",
        other => other,
    };
    let built = std::process::Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", package, "--bin", bin, "--profile", profile, "--target-dir"])
        .arg(profile_dir.parent().unwrap())
        .status()
        .unwrap();
    assert!(built.success());
    profile_dir.join(bin)
}

pub fn ask(h: &mut Helper, verb: Verb) -> Answer {
    let request = helper::HelperRequest { verb: Some(verb), ..Default::default() };
    helper::HelperResponse::decode_from_slice(&h.handle(&request.encode_to_vec()))
        .unwrap()
        .result
        .unwrap()
}

pub fn store(endpoint: &str) -> api::S3Access {
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

pub fn start(job: &str, work: Work, endpoint: &str) -> Verb {
    Verb::StartJob(Box::new(helper::StartJobRequest {
        job: job.into(),
        work: Some(work),
        data_key: vec![1; 32],
        store: store(endpoint).into(),
        concurrency: 4,
        ..Default::default()
    }))
}

pub fn status(h: &mut Helper, job: &str) -> helper::JobStatusResponse {
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

pub async fn finished(h: &mut Helper, job: &str) -> korpis_proto::korpis::backup::v1::JobResult {
    for _ in 0..600 {
        let s = status(h, job);
        if s.state.as_known() == Some(helper::JobState::JOB_STATE_FINISHED) {
            return s.result.into_option().unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {job} did not finish");
}

pub fn forget(h: &mut Helper, job: &str) -> Answer {
    ask(
        h,
        Verb::ForgetJob(Box::new(helper::ForgetJobRequest {
            job: job.into(),
            ..Default::default()
        })),
    )
}
