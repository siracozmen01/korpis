// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

/// One entry of a layer made for a test.
enum Item<'a> {
    Dir(&'a str),
    File(&'a str, &'a str, (u64, u64), u32),
    Symlink(&'a str, &'a str),
    Hard(&'a str, &'a str),
    Fifo(&'a str),
    Char(&'a str),
}

fn header(kind: tar::EntryType, path: &str, mode: u32, size: u64) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(kind);
    h.set_path(path).unwrap();
    h.set_mode(mode);
    h.set_size(size);
    h.set_mtime(1_700_000_000);
    h.set_uid(0);
    h.set_gid(0);
    h
}

fn layer(items: &[Item<'_>]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for item in items {
        let mut link = |kind, path: &str, target: &str| {
            let mut h = header(kind, path, 0o777, 0);
            b.append_link(&mut h, path, target).unwrap();
        };
        match item {
            Item::Dir(p) => {
                let mut h = header(tar::EntryType::Directory, p, 0o755, 0);
                h.set_cksum();
                b.append(&h, io::empty()).unwrap();
            }
            Item::File(p, data, (uid, gid), mode) => {
                let mut h =
                    header(tar::EntryType::Regular, p, *mode, u64::try_from(data.len()).unwrap());
                h.set_uid(*uid);
                h.set_gid(*gid);
                h.set_cksum();
                b.append(&h, data.as_bytes()).unwrap();
            }
            Item::Symlink(p, t) => link(tar::EntryType::Symlink, p, t),
            Item::Hard(p, t) => link(tar::EntryType::Link, p, t),
            Item::Fifo(p) | Item::Char(p) => {
                let kind = if matches!(item, Item::Fifo(_)) {
                    tar::EntryType::Fifo
                } else {
                    tar::EntryType::Char
                };
                let mut h = header(kind, p, 0o644, 0);
                h.set_cksum();
                b.append(&h, io::empty()).unwrap();
            }
        }
    }
    b.into_inner().unwrap()
}

fn root_of(dir: &Path) -> OwnedFd {
    rustix::fs::open(dir, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty()).unwrap()
}

fn is_root() -> bool {
    rustix::process::geteuid().is_root()
}

#[test]
fn entries_keep_their_owners_and_modes_and_links_resolve_within_the_tree() {
    if !is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = root_of(dir.path());
    let first = layer(&[
        Item::Dir("usr/"),
        Item::Dir("usr/lib/"),
        Item::File("usr/lib/libx", "x", (1000, 1000), 0o4755),
        Item::Symlink("lib", "/usr/lib"),
        Item::File("lib/liby", "y", (0, 0), 0o644),
        Item::Hard("usr/lib/libz", "usr/lib/libx"),
        Item::Fifo("run/pipe"),
        Item::Char("dev/null"),
    ]);
    let applied = apply(&root, first.as_slice()).unwrap();
    assert_eq!(applied.devices_skipped, 1, "a device is skipped, and counted");
    let at = |p: &str| std::fs::symlink_metadata(dir.path().join(p)).unwrap();
    let libx = at("usr/lib/libx");
    assert_eq!((libx.uid(), libx.gid(), libx.mode() & 0o7777), (1000, 1000, 0o4755));
    assert!(at("usr/lib/liby").is_file(), "a path through a symlink lands where it points");
    assert_eq!(at("usr/lib/libz").ino(), libx.ino(), "a hard link is the same file");
    assert!(at("run/pipe").file_type().is_fifo());
    assert!(!dir.path().join("dev/null").exists());
}

#[test]
fn a_header_whose_owner_and_time_are_left_blank_is_read_as_go_reads_it() {
    if !is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = root_of(dir.path());
    // As a layer written with a header's numbers left unset: containerd unpacks it, owned by 0.
    let mut h = tar::Header::new_gnu();
    h.set_path("bin/tool").unwrap();
    h.set_size(1);
    h.set_mode(0o755);
    h.set_cksum();
    let mut b = tar::Builder::new(Vec::new());
    b.append(&h, b"x".as_slice()).unwrap();
    apply(&root, b.into_inner().unwrap().as_slice()).unwrap();
    let tool = std::fs::metadata(dir.path().join("bin/tool")).unwrap();
    assert_eq!((tool.uid(), tool.gid(), tool.mode() & 0o7777, tool.mtime()), (0, 0, 0o755, 0));
}

#[test]
fn a_whiteout_removes_and_an_opaque_directory_hides_what_lower_layers_put_there() {
    if !is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = root_of(dir.path());
    let lower = layer(&[
        Item::File("etc/a", "a", (0, 0), 0o644),
        Item::File("etc/b", "b", (0, 0), 0o644),
        Item::File("etc/sub/c", "c", (0, 0), 0o644),
    ]);
    apply(&root, lower.as_slice()).unwrap();
    let upper = layer(&[
        Item::File("etc/own", "o", (0, 0), 0o644),
        Item::File("etc/.wh.own", "", (0, 0), 0o644),
        Item::File("etc/.wh.a", "", (0, 0), 0o644),
        Item::File("etc/sub/d", "d", (0, 0), 0o644),
        Item::File("etc/sub/.wh..wh..opq", "", (0, 0), 0o644),
    ]);
    apply(&root, upper.as_slice()).unwrap();
    let exists = |p: &str| dir.path().join(p).exists();
    assert!(!exists("etc/a") && exists("etc/b"), "the whiteout removed only its own name");
    assert!(exists("etc/own"), "a whiteout hides lower layers' entries, never its own layer's");
    assert!(!exists("etc/sub/c") && exists("etc/sub/d"), "opaque: the upper layer's alone");
    assert!(!exists("etc/.wh.a") && !exists("etc/sub/.wh..wh..opq"), "markers are not files");
}

#[test]
fn nothing_lands_outside_the_tree_and_ids_beyond_the_workload_s_are_refused() {
    if !is_root() {
        return;
    }
    let outside = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = root_of(dir.path());
    let climbing = format!("/../../../..{}", outside.path().display());
    let escape =
        layer(&[Item::Symlink("evil", &climbing), Item::File("evil/planted", "p", (0, 0), 0o644)]);
    apply(&root, escape.as_slice()).unwrap();
    assert!(!outside.path().join("planted").exists(), "a climbing link stays in the tree");
    assert!(dir.path().join(outside.path().strip_prefix("/").unwrap()).join("planted").exists());
    let foreign = layer(&[Item::File("x", "x", (70_000, 0), 0o644)]);
    assert!(apply(&root, foreign.as_slice()).is_err(), "an owner outside the 65536 ids");
    let hard = layer(&[Item::Hard("stolen", "../../etc/shadow")]);
    assert!(apply(&root, hard.as_slice()).is_err(), "a hard link that climbs is refused");
}
