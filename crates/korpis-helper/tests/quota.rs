// SPDX-License-Identifier: AGPL-3.0-or-later

//! Project quota in the write path, on a real ext4 made for the test and mounted with
//! `prjquota` from a loop device: a volume of 1 MiB and 64 inodes refuses the byte and the file
//! past either, with `EDQUOT`, to a writer without `CAP_SYS_RESOURCE` as a workload is; its usage
//! is read from the filesystem's accounting; deleting it lifts the limits. It needs root and
//! `mkfs.ext4`, and is skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

#[allow(dead_code, reason = "each test binary uses part of the shared fixture")]
mod fixture;

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

#[test]
fn a_volume_refuses_the_byte_and_the_file_past_its_limits() {
    let root = std::fs::metadata("/proc/self").unwrap().uid() == 0;
    let dir = tempfile::tempdir().unwrap();
    let Some(m) = root.then(|| fixture::ext4(dir.path())).flatten() else { return };
    let mut class = Class::open("fast", &m.0.join("volumes")).unwrap();
    assert_eq!(class.backend(), Backend::Ext4Project, "detected, not configured");
    assert!(class.describe().capabilities.as_option().unwrap().quota_enforced);

    let r = helper::EnsureVolumeRequest {
        volume: "wl_q.data".into(),
        size_bytes: 1 << 20,
        inodes: 64,
        ..Default::default()
    };
    let made = class.ensure(&r).unwrap();
    assert!(made.quota_enforced);
    let vol = PathBuf::from(&made.host_path);
    std::fs::set_permissions(&vol, std::fs::Permissions::from_mode(0o777)).unwrap();

    let big = as_nobody(&vol, "dd if=/dev/zero of=big bs=64k count=32 2>&1");
    let said = String::from_utf8_lossy(&big.stdout);
    assert!(!big.status.success() && said.contains("quota"), "EDQUOT: {said}");
    let many = as_nobody(&vol, "for i in $(seq 100); do : > f$i || exit 1; done 2>&1");
    assert!(!many.status.success(), "the 65th inode is refused");

    let u = class.usage("wl_q.data").unwrap();
    assert!(u.accounted && u.limit_bytes == 1 << 20 && u.limit_inodes == 64, "{u:?}");
    assert!(u.used_bytes > 0 && u.used_bytes <= 1 << 20, "read, and within the limit: {u:?}");

    assert!(class.delete("wl_q.data").unwrap());
    class.ensure(&r).unwrap();
    let fresh = class.usage("wl_q.data").unwrap();
    assert_eq!((fresh.used_inodes, fresh.used_bytes), (1, 4096), "its own directory alone");
}
