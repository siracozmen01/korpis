// SPDX-License-Identifier: AGPL-3.0-or-later

//! btrfs, on a real filesystem made for the test and mounted from a loop device: with qgroups on,
//! a volume is a subvolume whose limit refuses the byte past it, with `EDQUOT`, to a writer
//! without `CAP_SYS_RESOURCE` as a workload is; its usage is read from the qgroup accounting; a
//! volume attached again with no size keeps its limit; deleting it removes the subvolume. With
//! qgroups off the class enforces nothing and says so. It needs root and `mkfs.btrfs`, and is
//! skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

mod fixture;

use fixture::mounted;
use korpis_helper::storage::{Backend, Class};
use korpis_proto::korpis::helper::v1 as helper;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs `script` as nobody, without the capability that would let it pass the quota.
fn as_nobody(cwd: &Path, script: &str) -> std::process::Output {
    Command::new("setpriv")
        .args(["--reuid=65534", "--regid=65534", "--clear-groups", "--inh-caps=-all"])
        .args(["sh", "-c", script])
        .current_dir(cwd)
        .output()
        .unwrap()
}

/// A temporary directory nobody may traverse into, for the writes made as nobody.
fn scratch() -> Option<tempfile::TempDir> {
    let root = std::fs::metadata("/proc/self").unwrap().uid() == 0;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    root.then_some(dir)
}

#[test]
fn a_subvolume_refuses_the_byte_past_its_limit() {
    let Some(dir) = scratch() else { return };
    let Some(m) = mounted(dir.path(), true) else { return };
    let mut class = Class::open("snap", &m.0.join("volumes")).unwrap();
    assert_eq!(class.backend(), Backend::Btrfs, "detected, not configured");
    let caps = class.describe().capabilities.into_option().unwrap();
    assert!(caps.quota_enforced && caps.snapshot && caps.incremental_send && !caps.quota_inodes);

    let r = helper::EnsureVolumeRequest {
        volume: "wl_b.data".into(),
        size_bytes: 1 << 20,
        inodes: 64,
        ..Default::default()
    };
    let made = class.ensure(&r).unwrap();
    assert!(made.created && made.quota_enforced);
    let vol = PathBuf::from(&made.host_path);
    assert_eq!(std::fs::metadata(&vol).unwrap().ino(), 256, "a subvolume");
    std::fs::set_permissions(&vol, std::fs::Permissions::from_mode(0o777)).unwrap();
    let big = as_nobody(&vol, "dd if=/dev/zero of=big bs=64k count=32 2>&1");
    let said = String::from_utf8_lossy(&big.stdout);
    assert!(!big.status.success() && said.contains("quota"), "EDQUOT: {said}");
    assert!(Command::new("sync").status().unwrap().success());

    let u = class.usage("wl_b.data").unwrap();
    assert!(u.accounted && u.limit_bytes == 1 << 20, "{u:?}");
    assert!(u.used_bytes > 0 && u.used_bytes <= 1 << 20, "read, and within the limit: {u:?}");
    let attached = helper::EnsureVolumeRequest { volume: r.volume.clone(), ..Default::default() };
    assert!(!class.ensure(&attached).unwrap().created);
    assert_eq!(class.usage("wl_b.data").unwrap().limit_bytes, 1 << 20, "no size keeps it");

    assert!(class.delete("wl_b.data").unwrap());
    assert!(!vol.exists());
    assert!(!class.delete("wl_b.data").unwrap(), "already gone");
}

#[test]
fn without_qgroups_a_btrfs_class_enforces_nothing() {
    let Some(dir) = scratch() else { return };
    let Some(m) = mounted(dir.path(), false) else { return };
    let class = Class::open("plain", &m.0.join("volumes")).unwrap();
    assert_eq!(class.backend(), Backend::Directory, "it says what it cannot do");
    assert!(!class.describe().capabilities.into_option().unwrap().quota_enforced);
}

#[test]
fn a_rollback_restores_what_the_snapshot_held_and_keeps_the_limit() {
    let Some(dir) = scratch() else { return };
    let Some(m) = mounted(dir.path(), true) else { return };
    let mut class = Class::open("snap", &m.0.join("volumes")).unwrap();
    let r = helper::EnsureVolumeRequest {
        volume: "wl_s.data".into(),
        size_bytes: 8 << 20,
        ..Default::default()
    };
    let vol = PathBuf::from(class.ensure(&r).unwrap().host_path);
    std::fs::write(vol.join("world"), "day one").unwrap();
    assert!(class.snapshot("wl_s.data", "snp_1").unwrap());
    assert!(!class.snapshot("wl_s.data", "snp_1").unwrap(), "taken once");
    std::fs::write(vol.join("world"), "griefed").unwrap();
    std::fs::write(vol.join("since"), "x").unwrap();

    class.rollback("wl_s.data", "snp_1").unwrap();
    assert_eq!(std::fs::read_to_string(vol.join("world")).unwrap(), "day one");
    assert!(!vol.join("since").exists(), "what was written since is gone");
    assert!(std::fs::OpenOptions::new().write(true).open(vol.join("world")).is_ok(), "writable");
    assert!(Command::new("sync").status().unwrap().success());
    assert_eq!(class.usage("wl_s.data").unwrap().limit_bytes, 8 << 20, "its limit kept");
    class.rollback("wl_s.data", "snp_1").unwrap();
    assert_eq!(std::fs::read_to_string(vol.join("world")).unwrap(), "day one", "again, same");

    assert!(class.snapshot("wl_s.data", "snp_2").unwrap());
    assert!(class.delete_snapshot("wl_s.data", "snp_1").unwrap());
    assert!(!class.delete_snapshot("wl_s.data", "snp_1").unwrap(), "already gone");
    assert!(class.delete("wl_s.data").unwrap());
    let snapshots = m.0.join("volumes/.snapshots/wl_s.data");
    assert!(!snapshots.exists(), "a volume's snapshots go with it");
}

#[test]
fn a_class_that_cannot_snapshot_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut class = Class::open("plain", &dir.path().join("volumes")).unwrap();
    if class.backend() != Backend::Directory {
        return;
    }
    let r = helper::EnsureVolumeRequest { volume: "wl_d.data".into(), ..Default::default() };
    class.ensure(&r).unwrap();
    let refused = class.snapshot("wl_d.data", "snp_1").unwrap_err();
    assert!(refused.to_string().contains("cannot snapshot"), "{refused}");
}
