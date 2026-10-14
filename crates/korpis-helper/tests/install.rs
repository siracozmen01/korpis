// SPDX-License-Identifier: AGPL-3.0-or-later

//! Install jobs through the helper's verbs, with the real worker: a recipe's actions written into
//! a workload's volumes, one of them mounted inside the other, and owned as the workload's root
//! sees them; the worker confined and saying so; a nested volume's mount point that the tenant
//! made a link refused rather than followed; and an install that failed part way resumed at the
//! action that failed. It needs root, and is skipped without it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

#[allow(dead_code, reason = "each test binary uses part of the shared fixture")]
mod fixture;
#[allow(dead_code, reason = "each test binary uses part of the shared fixture")]
#[path = "fixture/jobs.rs"]
mod jobs;

use jobs::{ask, install_binary, job_binary, status};
use korpis_core::recipe::install::InstallStep;
use korpis_core::recipe::installing::{InstallAction, detail};
use korpis_core::workload::assignment::IdRange;
use korpis_helper::helper::Helper;
use korpis_helper::ids::Ledger;
use korpis_helper::job::JobConfig;
use korpis_helper::storage::Class;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::install::v1 as install;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A helper running install jobs over volumes `wl_1.data` and `wl_1.plugins`, with a content
/// store holding `motd.txt`'s bytes; the content's blobs directory.
fn setup(tmp: &Path) -> (Helper, PathBuf) {
    let mut class = Class::open("local", &tmp.join("volumes")).unwrap();
    for v in ["wl_1.data", "wl_1.plugins"] {
        let r = helper::EnsureVolumeRequest {
            volume: v.into(),
            workload: "wl_1".into(),
            size_bytes: 1 << 30,
            ..Default::default()
        };
        class.ensure(&r).unwrap();
    }
    let blobs = tmp.join("content/blobs");
    std::fs::create_dir_all(blobs.join("sha256")).unwrap();
    std::fs::create_dir_all(blobs.join("sha512")).unwrap();
    let config = JobConfig {
        binary: job_binary(),
        install_binary: Some(install_binary()),
        state: tmp.join("state"),
        cgroups: None,
        content: Some(blobs.clone()),
    };
    let mut ledger = Ledger::open(&tmp.join("state")).unwrap();
    ledger.ensure("wl_1", range()).unwrap();
    let h = Helper::new(vec![class], ledger).with_jobs(config);
    (h, blobs)
}

/// The id range `wl_1` holds.
fn range() -> IdRange {
    IdRange::of_slot(7).unwrap()
}

/// Keeps `bytes` in the content store at `blobs`; their digest.
fn keep(blobs: &Path, bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let hex: String = sha2::Sha256::digest(bytes).iter().fold(String::new(), |mut s, b| {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        s
    });
    std::fs::write(blobs.join("sha256").join(&hex), bytes).unwrap();
    format!("sha256:{hex}")
}

fn act(step: InstallStep) -> korpis_proto::korpis::v1::Value {
    korpis_wire::value::to_pb(&detail::encode(&InstallAction { step, context: None }))
}

fn write(path: &str, content: &str) -> korpis_proto::korpis::v1::Value {
    act(InstallStep::Write { path: path.into(), content: content.into(), mode: 0o644 })
}

/// Starts install job `job` of `actions` from `from`, with the recipe file `motd.txt` as `motd`.
fn start(
    h: &mut Helper,
    job: &str,
    (actions, from): (Vec<korpis_proto::korpis::v1::Value>, u32),
    motd: &str,
) {
    let volume = |path: &str, volume: &str| install::InstallVolume {
        path: path.into(),
        volume: volume.into(),
        ..Default::default()
    };
    let work = helper::InstallWork {
        actions,
        from,
        volumes: vec![volume("/data", "wl_1.data"), volume("/data/plugins", "wl_1.plugins")],
        files: [("motd.txt".to_owned(), motd.to_owned())].into(),
        workload: "wl_1".into(),
        id_base: range().base,
        ..Default::default()
    };
    let verb = Verb::StartJob(Box::new(helper::StartJobRequest {
        job: job.into(),
        work: Some(Work::Install(Box::new(work))),
        ..Default::default()
    }));
    ask(h, verb);
}

/// The job's status once it is no longer running.
async fn ended(h: &mut Helper, job: &str) -> helper::JobStatusResponse {
    for _ in 0..600 {
        let s = status(h, job);
        if s.state.as_known() != Some(helper::JobState::JOB_STATE_RUNNING) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("job {job} still running");
}

#[tokio::test]
async fn an_install_writes_into_the_workload_s_volumes_confined_to_them() {
    if !rustix::process::geteuid().is_root() {
        eprintln!("install jobs need root: not run");
        return;
    }
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let (mut h, blobs) = setup(tmp.path());
    let motd = keep(&blobs, b"welcome\n");
    let actions = vec![
        write("/data/eula.txt", "eula=true\n"),
        act(InstallStep::Copy { from: "motd.txt".into(), to: "/data/motd.txt".into() }),
        act(InstallStep::Mkdir { path: "/data/plugins/config".into(), mode: 0o750 }),
        act(InstallStep::Chown { path: "/data/eula.txt".into(), uid: 1000, gid: 1001 }),
        act(InstallStep::Symlink { target: "motd.txt".into(), link: "/data/latest".into() }),
    ];
    start(&mut h, "inst-1", (actions, 0), &motd);
    let s = ended(&mut h, "inst-1").await;
    let result = s.install.as_option().unwrap();
    assert!(result.ok, "{}", result.error);
    assert_eq!((result.done, s.progress), (5, 5));
    let data = tmp.path().join("volumes/wl_1.data");
    assert_eq!(std::fs::read(data.join("motd.txt")).unwrap(), b"welcome\n");
    let eula = std::fs::metadata(data.join("eula.txt")).unwrap();
    assert_eq!((eula.uid(), eula.gid()), (1000, 1001), "as the workload's root set it");
    assert_eq!(std::fs::metadata(data.join("motd.txt")).unwrap().uid(), 0, "the workload's root");
    let plugins = tmp.path().join("volumes/wl_1.plugins");
    assert!(plugins.join("config").is_dir(), "into the volume mounted inside the other");
    assert!(data.join("plugins").is_dir() && !data.join("plugins/config").exists());
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    let here = tmp.path().to_str().unwrap();
    assert!(!mounts.contains(here), "no mount of the worker's is seen from the host:\n{mounts}");
    let held = result.confinement.as_option().unwrap();
    assert!(held.namespaces && held.own_ids && held.no_new_privs, "{held:?}");
    let landlock = std::fs::read_to_string("/sys/kernel/security/lsm")
        .is_ok_and(|lsm| lsm.split(',').any(|m| m.trim() == "landlock"));
    assert_eq!(held.landlock, landlock, "enforced wherever the kernel offers it");
    assert_eq!(held.capabilities, ["chown", "fowner", "dac_override", "fsetid"]);
}

#[tokio::test]
async fn a_nested_volume_s_mount_point_made_a_link_is_refused_not_followed() {
    if !rustix::process::geteuid().is_root() {
        return;
    }
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let (mut h, blobs) = setup(tmp.path());
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, tmp.path().join("volumes/wl_1.data/plugins")).unwrap();
    let motd = keep(&blobs, b"x");
    start(&mut h, "inst-2", (vec![write("/data/plugins/x", "x")], 0), &motd);
    let s = ended(&mut h, "inst-2").await;
    let result = s.install.as_option().unwrap();
    assert!(!result.ok && result.error.contains("plugins"), "{result:?}");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing mounted or written there");
}

#[tokio::test]
async fn an_install_that_failed_part_way_resumes_at_the_action_that_failed() {
    if !rustix::process::geteuid().is_root() {
        return;
    }
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let (mut h, blobs) = setup(tmp.path());
    let later = "sha256:".to_owned() + &"ab".repeat(32);
    let actions = vec![
        write("/data/a", "a"),
        act(InstallStep::Copy { from: "motd.txt".into(), to: "/data/b".into() }),
        write("/data/c", "c"),
    ];
    start(&mut h, "inst-3", (actions.clone(), 0), &later);
    let s = ended(&mut h, "inst-3").await;
    let result = s.install.as_option().unwrap();
    assert!(!result.ok);
    assert_eq!((result.done, s.progress), (1, 1), "the first ran, the second did not");
    let motd = keep(&blobs, b"now here");
    start(&mut h, "inst-4", (actions, s.progress), &motd);
    let s = ended(&mut h, "inst-4").await;
    assert!(s.install.as_option().unwrap().ok);
    assert_eq!(s.progress, 3);
    let data = tmp.path().join("volumes/wl_1.data");
    assert_eq!(std::fs::read(data.join("b")).unwrap(), b"now here");
    assert!(data.join("c").exists());
}
