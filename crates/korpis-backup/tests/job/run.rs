// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-backup-job` run as the helper runs it: a backup of a directory to MinIO, then a
//! restore of it into an empty one, each a process given its spec on standard input, each
//! leaving its result where the spec says, the restore verified and both confined.

use buffa::Message as _;
use korpis_backup::testing::Minio;
use korpis_proto::korpis::backup::v1 as pb;
use korpis_proto::korpis::backup::v1::__buffa::oneof::job_spec::Work as W;
use korpis_proto::korpis::v1 as api;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

fn store(m: &Minio) -> api::S3Access {
    let c = m.access("backups", "repo/workloads/wl_1/");
    api::S3Access {
        endpoint: c.endpoint,
        region: c.region,
        bucket: c.bucket,
        key_prefix: c.key_prefix,
        access_key_id: c.access_key_id,
        secret_access_key: c.secret_access_key,
        path_style: true,
        ..Default::default()
    }
}

/// Runs the job binary with `spec`; its result.
fn run(spec: &pb::JobSpec) -> pb::JobResult {
    let mut child = Command::new(env!("CARGO_BIN_EXE_korpis-backup-job"))
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&spec.encode_to_vec()).unwrap();
    assert!(child.wait().unwrap().success());
    pb::JobResult::decode_from_slice(&std::fs::read(&spec.result).unwrap()).unwrap()
}

fn spec(m: &Minio, work: W, directory: &Path, state: &Path) -> pb::JobSpec {
    pb::JobSpec {
        id: "job-1".into(),
        work: Some(work),
        data_key: vec![5; 32],
        store: store(m).into(),
        concurrency: 4,
        directory: directory.to_string_lossy().into_owned(),
        result: state.join("result").to_string_lossy().into_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_job_backs_up_and_restores_through_the_store_confined() {
    if !rustix::process::geteuid().is_root() {
        eprintln!("a job confines itself, which needs root: not run");
        return;
    }
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let Some(m) = Minio::start(tmp).await.unwrap() else { return };
    m.bucket("backups").await.unwrap();
    let dirs: Vec<tempfile::TempDir> = (0..4).map(|_| tempfile::tempdir_in(tmp).unwrap()).collect();
    let (src, dst, s1, s2) = (dirs[0].path(), dirs[1].path(), dirs[2].path(), dirs[3].path());
    std::fs::create_dir(src.join("world")).unwrap();
    std::fs::write(src.join("world/level.dat"), vec![9u8; 1 << 20]).unwrap();
    std::fs::write(src.join("eula.txt"), "eula=true\n").unwrap();

    let work = W::Backup(Box::new(pb::BackupWork {
        backup: "b1".into(),
        consistent: true,
        ..Default::default()
    }));
    let backed = run(&spec(&m, work, src, s1));
    assert!(backed.ok, "{}", backed.error);
    assert_eq!(backed.backup.as_option().unwrap().id, "b1");
    let held = backed.confinement.as_option().unwrap();
    assert!(held.landlock_filesystem && held.landlock_network);
    assert_eq!(held.capabilities, ["dac_read_search"]);

    let work = W::Restore(Box::new(pb::RestoreWork { backup: "b1".into(), ..Default::default() }));
    let restored = run(&spec(&m, work, dst, s2));
    assert!(restored.ok, "{}", restored.error);
    let r = restored.restored.as_option().unwrap();
    assert!(r.verified && r.files == 2, "{r:?}");
    assert_eq!(std::fs::read_to_string(dst.join("eula.txt")).unwrap(), "eula=true\n");
    assert!(restored.confinement.as_option().unwrap().capabilities.contains(&"chown".to_owned()));
}
