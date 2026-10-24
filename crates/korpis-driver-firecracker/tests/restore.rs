// SPDX-License-Identifier: AGPL-3.0-or-later

//! A path of a block device's backup restored into its live volume in a live guest (§5.4 of
//! docs/06-storage.md, §4.4 of docs/04-runtimes.md): the image the node staged is held by a
//! service guest of the workload's beside its volume, attached writable so its journal replays
//! and mounted read-only, and the install job copies the path from it into a new directory of the
//! volume with its modes; asked again it is done; and the workload, started, reads it. Needs what
//! the driver's runtime tests need.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

use korpis_driver_firecracker::machine::{Install, InstallStatus, Machine, Restore};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::install::v1 as pb;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

mod support;

use support::{Rig, live, no_service_guest, ok, ran, until};

/// How install `job` stands once it no longer runs.
async fn ended(machine: &Machine, job: &str) -> InstallStatus {
    for _ in 0..600 {
        let s = machine.install_status(job).await;
        if s.state != driver::InstallState::INSTALL_STATE_RUNNING {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("restore {job} still runs");
}

/// A backup's image staged beside the volumes, holding `saves/world/level.dat`, its journal
/// flagged as needing replay, owned by the guest's own id `base`; its path.
fn staged(rig: &Rig, base: u32) -> String {
    use korpis_core::workload::volume::{BLOCK_IMAGE, BLOCK_MARK};
    let dir = rig.dir.path().join("volumes/.wl_rst.data.read+read-1");
    std::fs::create_dir(&dir).unwrap();
    rustix::fs::setxattr(&dir, BLOCK_MARK.0, BLOCK_MARK.1, rustix::fs::XattrFlags::empty())
        .unwrap();
    let files = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(files.path().join("saves/world")).unwrap();
    std::fs::write(files.path().join("saves/world/level.dat"), b"seed 42").unwrap();
    let image = dir.join(BLOCK_IMAGE);
    ok(Command::new("mkfs.ext4").args(["-q", "-F", "-d"]).arg(files.path()).arg(&image).arg("16M"));
    ok(Command::new("debugfs").args(["-w", "-R", "feature needs_recovery"]).arg(&image));
    std::os::unix::fs::chown(&image, Some(base), Some(base)).unwrap();
    image.to_string_lossy().into_owned()
}

#[tokio::test]
async fn a_path_of_a_backup_s_image_is_restored_into_a_stopped_guest_s_volume() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let spec = rig.spec("wl_rst", "while true; do sleep 1; done");
    let image = driver::StagedImage {
        image: staged(&rig, spec.uid_base),
        workload: "wl_rst".into(),
        uid_base: spec.uid_base,
        uid_count: spec.uid_count,
        filesystem: "ext4".into(),
        ..Default::default()
    };
    let tree = pb::RestoreTree {
        from: "saves/world".into(),
        volume: "/data".into(),
        into: "restored".into(),
        ..Default::default()
    };
    let asked = Install {
        job: "rst-1".into(),
        spec: spec.clone(),
        actions: Vec::new(),
        from: 0,
        files: BTreeMap::default(),
        fetched: Vec::new(),
        restore: Some(Restore { image, tree }),
    };
    let id = rig.machine.create(spec).await.unwrap();
    rig.machine.start_install(asked.clone()).await.unwrap();
    let s = ended(&rig.machine, "rst-1").await;
    let result = s.result.unwrap();
    assert!(result.ok, "{}", result.error);
    assert_eq!(result.done, 1);
    let confined = result.confinement.into_option().unwrap();
    assert!(confined.namespaces && confined.landlock && confined.own_ids, "{confined:?}");
    no_service_guest(&rig.machine).await;

    // Asked again under the same job once forgotten, it finds its own work and is done.
    rig.machine.forget_install("rst-1").await;
    rig.machine.start_install(asked).await.unwrap();
    let again = ended(&rig.machine, "rst-1").await.result.unwrap();
    assert!(again.ok, "{}", again.error);
    no_service_guest(&rig.machine).await;

    rig.machine.start(&id).await.unwrap();
    let running = driver::RuntimeState::RUNTIME_STATE_RUNNING;
    until(&rig.machine, &id, running, Duration::from_secs(30)).await;
    let (said, _) = ran(&rig.machine, &id, &["/bin/cat", "/data/restored/world/level.dat"]).await;
    assert_eq!(said, "seed 42", "the started workload reads the restored path");
    rig.machine.destroy(&id).await.unwrap();
}
