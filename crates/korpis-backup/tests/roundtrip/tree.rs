// SPDX-License-Identifier: AGPL-3.0-or-later

//! The volume the round-trip tests back up: every kind of thing a volume holds that a backup must
//! bring back, and, as root, what only root can make.

use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;

/// Whether the test runs as root, and so can make devices and give files to other owners.
pub fn root() -> bool {
    rustix::process::geteuid().is_root()
}

/// Bytes that do not repeat, so chunks are not deduplicated within a file by accident.
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[0]
        })
        .collect()
}

fn set_mtime(path: &Path, seconds: i64) {
    let t = rustix::fs::Timespec { tv_sec: seconds, tv_nsec: 123_456_789 };
    let times = rustix::fs::Timestamps { last_access: t, last_modification: t };
    rustix::fs::utimensat(rustix::fs::CWD, path, &times, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .unwrap();
}

/// A POSIX access ACL granting user 1001 read, as `system.posix_acl_access` holds it.
fn acl() -> Vec<u8> {
    let mut v = 2u32.to_le_bytes().to_vec();
    for (tag, perm, id) in [
        (1u16, 6u16, u32::MAX),
        (2, 4, 1001),
        (4, 4, u32::MAX),
        (0x10, 4, u32::MAX),
        (0x20, 0, u32::MAX),
    ] {
        v.extend_from_slice(&tag.to_le_bytes());
        v.extend_from_slice(&perm.to_le_bytes());
        v.extend_from_slice(&id.to_le_bytes());
    }
    v
}

/// Makes the volume in `at`.
pub fn make(at: &Path) {
    fs::create_dir_all(at.join("world/region")).unwrap();
    fs::write(at.join("server.properties"), "motd=hello\n").unwrap();
    fs::write(at.join("world/level.dat"), noise(3 << 20, 1)).unwrap();
    fs::write(at.join("world/region/r.0.0.mca"), noise(6 << 20, 2)).unwrap();
    fs::write(at.join("empty"), b"").unwrap();
    let sparse = fs::File::create(at.join("sparse.img")).unwrap();
    sparse.set_len(64 << 20).unwrap();
    std::os::unix::fs::FileExt::write_all_at(&sparse, &noise(1 << 20, 3), 32 << 20).unwrap();
    let hole = fs::File::create(at.join("hole.img")).unwrap();
    hole.set_len(8 << 20).unwrap();
    fs::hard_link(at.join("server.properties"), at.join("world/properties.link")).unwrap();
    symlink("../server.properties", at.join("world/props")).unwrap();
    symlink("/nowhere/at/all", at.join("dangling")).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        at.join("pipe"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o640),
        0,
    )
    .unwrap();
    let f = fs::File::open(at.join("server.properties")).unwrap();
    rustix::fs::fsetxattr(&f, "user.korpis", b"kept", rustix::fs::XattrFlags::empty()).unwrap();
    fs::set_permissions(at.join("world/level.dat"), fs::Permissions::from_mode(0o640)).unwrap();
    if root() {
        rustix::fs::mknodat(
            rustix::fs::CWD,
            at.join("null"),
            rustix::fs::FileType::CharacterDevice,
            rustix::fs::Mode::from_raw_mode(0o666),
            rustix::fs::makedev(1, 3),
        )
        .unwrap();
        let run = at.join("run.sh");
        fs::write(&run, "#!/bin/sh\n").unwrap();
        std::os::unix::fs::chown(&run, Some(1000), Some(1000)).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o4755)).unwrap();
        let f = fs::File::open(at.join("world/level.dat")).unwrap();
        rustix::fs::fsetxattr(
            &f,
            "system.posix_acl_access",
            &acl(),
            rustix::fs::XattrFlags::empty(),
        )
        .unwrap();
        std::os::unix::fs::lchown(at.join("world/props"), Some(1000), Some(1001)).unwrap();
    }
    for (p, t) in [
        ("world/region/r.0.0.mca", 1_600_000_000),
        ("world/region", 1_600_000_100),
        ("world", 1_600_000_200),
        ("dangling", 1_600_000_300),
    ] {
        set_mtime(&at.join(p), t);
    }
    set_mtime(at, 1_600_000_400);
}
