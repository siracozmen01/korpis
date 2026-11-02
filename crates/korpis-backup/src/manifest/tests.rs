// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use std::os::unix::fs::FileExt as _;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

fn open(dir: &Path) -> OwnedFd {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    rustix::fs::open(dir, flags, Mode::empty()).unwrap()
}

/// A tree holding what a copy is known to lose: a file, a symlink, a second link to the file, a
/// sparse file, an extended attribute and a nested directory, each with a fixed time.
fn tree(root: &Path) {
    std::fs::create_dir(root.join("world")).unwrap();
    let level = root.join("world/level.dat");
    std::fs::write(&level, b"seed=7").unwrap();
    std::fs::hard_link(&level, root.join("world/level.bak")).unwrap();
    std::os::unix::fs::symlink("world/level.dat", root.join("current")).unwrap();
    let sparse = File::create(root.join("region.mca")).unwrap();
    sparse.set_len(1 << 20).unwrap();
    sparse.write_at(b"chunk", 1 << 19).unwrap();
    rustix::fs::setxattr(&level, "user.korpis", b"kept", rustix::fs::XattrFlags::empty()).unwrap();
    for f in ["world/level.dat", "region.mca"] {
        let at = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        File::options().write(true).open(root.join(f)).unwrap().set_modified(at).unwrap();
    }
}

fn manifest(root: &Path) -> Manifest {
    of(&open(root)).unwrap()
}

#[test]
fn two_trees_alike_have_one_manifest_and_whatever_a_copy_loses_changes_it() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    tree(a.path());
    tree(b.path());
    let same = manifest(a.path());
    assert_eq!(same, manifest(b.path()), "written alike, at other paths and times");
    assert_eq!(same.chunks.len(), 1);

    let losses: [(&str, fn(&Path)); 5] = [
        ("a symlink's target", |r| {
            std::fs::remove_file(r.join("current")).unwrap();
            std::os::unix::fs::symlink("world/level.bak", r.join("current")).unwrap();
        }),
        ("an extended attribute", |r| {
            rustix::fs::removexattr(&r.join("world/level.dat"), "user.korpis").unwrap();
        }),
        ("a hole", |r| {
            let f = File::options().write(true).open(r.join("region.mca")).unwrap();
            f.write_at(&[0; 4096], 0).unwrap();
            f.set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)).unwrap();
        }),
        ("a hard link", |r| {
            std::fs::remove_file(r.join("world/level.bak")).unwrap();
            std::fs::write(r.join("world/level.bak"), b"seed=7").unwrap();
        }),
        ("a modification time", |r| {
            let f = File::options().write(true).open(r.join("world/level.dat")).unwrap();
            f.set_modified(UNIX_EPOCH).unwrap();
        }),
    ];
    for (lost, lose) in losses {
        let c = tempfile::tempdir().unwrap();
        tree(c.path());
        lose(c.path());
        assert_ne!(manifest(c.path()).digest, same.digest, "{lost} lost is a difference");
    }
}

fn write_at_a_fixed_time(path: &Path, data: &[u8]) {
    std::fs::write(path, data).unwrap();
    let at = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    File::options().write(true).open(path).unwrap().set_modified(at).unwrap();
}

#[test]
fn a_difference_is_found_to_its_run() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for root in [a.path(), b.path()] {
        for n in 0..(RUN * 2) {
            write_at_a_fixed_time(&root.join(format!("{n:05}")), b"x");
        }
    }
    write_at_a_fixed_time(&b.path().join("01500"), b"y");
    let (left, right) = (manifest(a.path()), manifest(b.path()));
    assert_eq!(left.chunks.len(), 3, "the root and two thousand and forty-eight files");
    assert_eq!(left.chunks[0], right.chunks[0], "the first run is alike");
    assert_ne!(left.chunks[1], right.chunks[1]);
    assert_eq!(left.chunks[1].first_path, "01023", "the run the difference is in");
}
