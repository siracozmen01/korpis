// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block-device volume (§3.1 of docs/06-storage.md), on a real ext4 with project quota from a
//! loop device: it is made as a sparse image of its size, formatted empty as ext4 owned by the
//! workload's root, and never mounted; asked again it is found, grown when asked for more and
//! refused when asked for less; a volume of files is never made a block device or the reverse;
//! no file session or install opens it, and nothing is restored beside its image; its quota leaves
//! its image room; and it is deleted with its directory. It needs root and the e2fsprogs, and is
//! skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

#[allow(dead_code, reason = "each test binary uses part of the shared fixture")]
mod fixture;

use korpis_helper::block::{IMAGE, files_of, is_block, restorable};
use korpis_helper::storage::Class;
use korpis_proto::korpis::helper::v1 as helper;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::process::Command;

const MIB: u64 = 1 << 20;

/// What `debugfs` says of the image at `image` when asked `request`.
fn debugfs(image: &Path, request: &str) -> String {
    let out = Command::new("debugfs").args(["-R", request]).arg(image).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn block(volume: &str, size_bytes: u64) -> helper::EnsureVolumeRequest {
    helper::EnsureVolumeRequest {
        volume: volume.into(),
        size_bytes,
        block_device: true,
        ..Default::default()
    }
}

#[test]
fn a_block_device_is_an_empty_ext4_of_its_size_grown_and_never_shrunk() {
    let root = std::fs::metadata("/proc/self").unwrap().uid() == 0;
    let dir = tempfile::tempdir().unwrap();
    let Some(m) = root.then(|| fixture::ext4(dir.path())).flatten() else { return };
    let mut class = Class::open("fast", &m.0.join("volumes")).unwrap();

    let made = class.ensure(&block("wl_b.disk", 16 * MIB)).unwrap();
    assert!(made.created && made.quota_enforced, "its size is its bound: {made:?}");
    let image = Path::new(&made.host_path);
    assert_eq!(image.file_name().unwrap(), IMAGE);
    let meta = std::fs::metadata(image).unwrap();
    assert_eq!(meta.len(), 16 * MIB);
    assert!(meta.blocks() * 512 < 16 * MIB, "sparse, not written out");
    assert!(is_block(image.parent().unwrap()));
    let root_dir = debugfs(image, "stat /");
    assert!(root_dir.contains("User:     0") && root_dir.contains("Group:     0"), "{root_dir}");

    let again = class.ensure(&block("wl_b.disk", 0)).unwrap();
    assert!(!again.created && again.host_path == made.host_path, "found, as it is");
    class.ensure(&block("wl_b.disk", 32 * MIB)).unwrap();
    assert_eq!(std::fs::metadata(image).unwrap().len(), 32 * MIB, "grown");
    let smaller = class.ensure(&block("wl_b.disk", 8 * MIB));
    assert!(smaller.unwrap_err().to_string().contains("cannot shrink"));

    let files = helper::EnsureVolumeRequest { volume: "wl_b.disk".into(), ..Default::default() };
    assert!(class.ensure(&files).is_err(), "a block device is not made a volume of files");
    let plain = helper::EnsureVolumeRequest { volume: "wl_b.data".into(), ..Default::default() };
    let data = class.ensure(&plain).unwrap().host_path;
    let data = std::path::PathBuf::from(data);
    assert!(restorable("wl_b.data", &data, ("world", "restored")).is_ok(), "into a directory");
    assert!(files_of("wl_b.data", data).is_ok(), "a volume of files is opened");
    let disk = image.parent().unwrap().to_owned();
    for (path, into) in [("world", "restored"), ("", "restored"), ("world", "")] {
        let beside = restorable("wl_b.disk", &disk, (path, into));
        assert!(beside.is_err(), "nothing restored beside a block device's image: {path} {into}");
    }
    assert!(restorable("wl_b.disk", &disk, ("", "")).is_ok(), "its whole image, from_backup");
    assert!(files_of("wl_b.disk", disk).is_err(), "a block device's files are its guest's");
    assert!(class.ensure(&block("wl_b.data", 16 * MIB)).is_err(), "nor files a block device");
    assert!(class.ensure(&block("wl_b.none", 0)).is_err(), "a block device needs a size");

    let u = class.usage("wl_b.disk").unwrap();
    assert!(u.accounted && u.allocated, "{u:?}");
    assert!(u.limit_bytes > 32 * MIB && u.used_bytes < 32 * MIB, "{u:?}");
    assert!(!class.usage("wl_b.data").unwrap().allocated, "a volume of files holds what it says");
    assert!(class.delete("wl_b.disk").unwrap());
    assert!(!image.exists());
}

#[test]
fn a_replacing_restore_of_a_block_device_leaves_a_block_device() {
    let root = std::fs::metadata("/proc/self").unwrap().uid() == 0;
    let dir = tempfile::tempdir().unwrap();
    let Some(m) = root.then(|| fixture::ext4(dir.path())).flatten() else { return };
    let mut class = Class::open("fast", &m.0.join("volumes")).unwrap();
    let made = class.ensure(&block("wl_r.disk", 16 * MIB)).unwrap();

    let (staged, identity, limits) = class.stage("wl_r.disk").unwrap();
    assert!(is_block(&staged), "the staging place is a block device's, as its volume is");
    // The restore job writes the backup's one file, the image, into it.
    std::fs::copy(&made.host_path, staged.join(IMAGE)).unwrap();
    let s = korpis_helper::staging::Staging { volume: "wl_r.disk", identity, limits };
    class.settle(&s, true).unwrap();

    let volume = Path::new(&made.host_path).parent().unwrap();
    assert!(is_block(volume), "the replaced volume is still a block device");
    let again = class.ensure(&block("wl_r.disk", 0)).unwrap();
    assert!(!again.created && again.host_path == made.host_path, "and is found as one");
}
