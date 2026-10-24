// SPDX-License-Identifier: AGPL-3.0-or-later

//! A path of a backup restored into a new directory of a volume (§5.4 of docs/06-storage.md),
//! between two real directories: a tree is copied under its own name with its files' data and
//! holes, its links as links and never followed, and its modes, owners, and times; asked again as
//! the same job it finds its own work; a directory already there is refused; a tree holding a
//! fifo is refused with nothing left at the target; and the backup's root and a path that climbs
//! are refused.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

use korpis_install::tree::copy_tree;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

fn open(dir: &Path) -> OwnedFd {
    let flags = rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY;
    rustix::fs::open(dir, flags, rustix::fs::Mode::empty()).unwrap()
}

/// A world: a file with a hole in it, a nested directory, a link out of the tree, and odd modes.
fn world(root: &Path) {
    let w = root.join("saves/world");
    std::fs::create_dir_all(w.join("region")).unwrap();
    std::fs::write(w.join("level.dat"), b"seed 42").unwrap();
    let sparse = std::fs::File::create(w.join("region/r.0.0.mca")).unwrap();
    sparse.set_len(8 << 20).unwrap();
    std::os::unix::fs::FileExt::write_at(&sparse, b"chunk", 4 << 20).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", w.join("out")).unwrap();
    std::fs::set_permissions(w.join("level.dat"), std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::set_permissions(w.join("region"), std::fs::Permissions::from_mode(0o710)).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(w.join("level.dat"))
        .unwrap()
        .set_modified(old)
        .unwrap();
}

#[test]
fn a_path_of_a_backup_is_copied_whole_into_a_new_directory_and_once() {
    let (source, target) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    world(source.path());
    let (from, to) = (open(source.path()), open(target.path()));

    let copied = copy_tree((&from, "saves/world"), (&to, "restored"), "rst-1").unwrap();
    assert_eq!(copied.entries, 5, "the world, its level, its regions, a region, and a link");
    let w = target.path().join("restored/world");
    assert_eq!(std::fs::read(w.join("level.dat")).unwrap(), b"seed 42");
    let level = std::fs::metadata(w.join("level.dat")).unwrap();
    assert_eq!(level.mode() & 0o7777, 0o640);
    assert_eq!(level.mtime(), 1_000_000_000, "its time kept");
    assert_eq!(std::fs::metadata(w.join("region")).unwrap().mode() & 0o7777, 0o710);
    let region = std::fs::metadata(w.join("region/r.0.0.mca")).unwrap();
    assert_eq!(region.len(), 8 << 20);
    assert!(region.blocks() * 512 < 1 << 20, "its holes kept: {} blocks", region.blocks());
    let mut chunk = [0u8; 5];
    let f = std::fs::File::open(w.join("region/r.0.0.mca")).unwrap();
    std::os::unix::fs::FileExt::read_exact_at(&f, &mut chunk, 4 << 20).unwrap();
    assert_eq!(&chunk, b"chunk");
    assert_eq!(std::fs::read_link(w.join("out")).unwrap(), Path::new("/etc/passwd"));
    let link = std::fs::symlink_metadata(w.join("out")).unwrap();
    assert!(link.file_type().is_symlink(), "a link, never followed");

    let again = copy_tree((&from, "saves/world"), (&to, "restored"), "rst-1").unwrap();
    assert_eq!(again.entries, 0, "the same job finds its own work and is done");
    let other = copy_tree((&from, "saves/world"), (&to, "restored"), "rst-2");
    assert!(other.unwrap_err().contains("there already"), "a restore never merges");
    let names: Vec<_> = std::fs::read_dir(target.path()).unwrap().flatten().collect();
    assert_eq!(names.len(), 1, "nothing left beside it");
}

#[test]
fn what_is_not_a_file_a_directory_or_a_link_is_refused_and_nothing_left() {
    let (source, target) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    world(source.path());
    let fifo = source.path().join("saves/world/pipe");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    let (from, to) = (open(source.path()), open(target.path()));
    let refused = copy_tree((&from, "saves"), (&to, "restored"), "rst-3").unwrap_err();
    assert!(refused.contains("pipe"), "{refused}");
    let left: Vec<_> = std::fs::read_dir(target.path()).unwrap().flatten().collect();
    assert!(left.is_empty(), "no partial target, and nothing beside it: {left:?}");
    for bad in ["", "/", "saves/../..", "saves/./world"] {
        assert!(copy_tree((&from, bad), (&to, "x"), "rst-4").is_err(), "{bad:?}");
    }
    assert!(copy_tree((&from, "saves"), (&to, "a/b"), "rst-5").is_err(), "a name at the root");
}
