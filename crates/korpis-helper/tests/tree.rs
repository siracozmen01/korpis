// SPDX-License-Identifier: AGPL-3.0-or-later

//! A migration's tree passes between two classes on plain directories, standing for two nodes
//! (§8.1 of docs/05-scheduling.md): rsync's daemon side sends the source's volume, read only and
//! confined to it, to the client pulling into the destination's copy, over the two sockets as the
//! agents would join them, keeping a symlink, a hardlink, an extended attribute, an ACL, a sparse
//! file's holes, and an owner by number; a second pass sends what changed and deletes what the
//! source no longer has, and the receiving side says what crossed. It needs root and rsync, and is
//! skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

use korpis_helper::storage::Class;
use korpis_helper::tree::{RSYNC, TreeEnd};
use korpis_proto::korpis::helper::v1 as helper;
use std::os::unix::fs::{MetadataExt as _, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

const VOLUME: &str = "wl_t.data";

/// The helper's binary, rsync's remote shell.
fn relay() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_korpis-helper"))
}

/// Joins two sockets both ways until each side closes.
fn join(a: rustix::fd::OwnedFd, b: rustix::fd::OwnedFd) {
    let (mut a_in, mut b_out) = (std::fs::File::from(a), std::fs::File::from(b));
    let (mut a_out, mut b_in) = (a_in.try_clone().unwrap(), b_out.try_clone().unwrap());
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut a_in, &mut b_out);
        let _ = rustix::net::shutdown(&b_out, rustix::net::Shutdown::Write);
    });
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut b_in, &mut a_out);
        let _ = rustix::net::shutdown(&a_out, rustix::net::Shutdown::Write);
    });
}

/// One pass from `from` to `to`; what each side came to, sender first.
fn pass(from: &Class, to: &Class) -> (TreeEnd, TreeEnd) {
    let (send, mut sender) = from.send_tree(VOLUME).unwrap();
    let (receive, mut receiver) = to.receive_tree(VOLUME, &relay()).unwrap();
    join(send, receive);
    let wait = |class: &Class, child: &mut std::process::Child, sending| {
        for _ in 0..600 {
            match class.tree_ended(VOLUME, child, sending).unwrap() {
                TreeEnd::Running => std::thread::sleep(Duration::from_millis(50)),
                came => return came,
            }
        }
        panic!("the pass never ended");
    };
    (wait(from, &mut sender, true), wait(to, &mut receiver, false))
}

fn volume(class: &mut Class) -> PathBuf {
    let r = helper::EnsureVolumeRequest {
        volume: VOLUME.into(),
        size_bytes: 64 << 20,
        ..Default::default()
    };
    PathBuf::from(class.ensure(&r).unwrap().host_path)
}

/// An access ACL granting user 1000456 read, as the kernel stores it: version 2, then each entry's
/// tag, permissions, and id, little-endian.
fn acl() -> Vec<u8> {
    let entry = |tag: u16, perm: u16, id: u32| {
        [tag.to_le_bytes().as_slice(), &perm.to_le_bytes(), &id.to_le_bytes()].concat()
    };
    let none = u32::MAX;
    [
        2_u32.to_le_bytes().to_vec(),
        entry(0x01, 6, none),
        entry(0x02, 4, 1_000_456),
        entry(0x04, 4, none),
        entry(0x10, 4, none),
        entry(0x20, 4, none),
    ]
    .concat()
}

/// Fills `at` with what a game server's world might hold that a naive copy loses; false where
/// its filesystem keeps no extended attributes or ACLs.
fn fill(at: &Path) -> bool {
    use rustix::fs::{XattrFlags, setxattr};
    std::fs::write(at.join("level.dat"), "world").unwrap();
    symlink("level.dat", at.join("latest")).unwrap();
    std::fs::hard_link(at.join("level.dat"), at.join("level.bak")).unwrap();
    let sparse = std::fs::File::create(at.join("region.mca")).unwrap();
    sparse.set_len(32 << 20).unwrap();
    std::os::unix::fs::chown(at.join("level.dat"), Some(1_000_123), Some(1_000_123)).unwrap();
    let file = at.join("level.dat");
    setxattr(&file, "user.korpis", b"kept", XattrFlags::empty()).is_ok()
        && setxattr(&file, "system.posix_acl_access", &acl(), XattrFlags::empty()).is_ok()
}

/// The extended attribute `name` of `path`.
fn xattr(path: &Path, name: &str) -> Vec<u8> {
    let mut value = vec![0_u8; 256];
    let n = rustix::fs::getxattr(path, name, &mut value).unwrap();
    value.truncate(n);
    value
}

#[test]
fn a_tree_crosses_whole_then_by_its_differences_keeping_what_a_copy_loses() {
    let root = rustix::process::geteuid().is_root();
    if !root || std::fs::metadata(RSYNC).is_err() {
        return;
    }
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut source = Class::open("local", &one.path().join("volumes")).unwrap();
    let mut destination = Class::open("local", &two.path().join("volumes")).unwrap();
    let (from, to) = (volume(&mut source), volume(&mut destination));
    if !fill(&from) {
        return; // setfattr or setfacl is missing, or the filesystem refuses them
    }

    let (sent, received) = pass(&source, &destination);
    assert_eq!(sent, TreeEnd::Done(0));
    let TreeEnd::Done(whole) = received else { panic!("{received:?}") };
    assert!(whole > 0, "it says what crossed");
    let got = |name: &str| std::fs::symlink_metadata(to.join(name)).unwrap();
    assert_eq!(std::fs::read_link(to.join("latest")).unwrap(), Path::new("level.dat"));
    assert_eq!(got("level.dat").ino(), got("level.bak").ino(), "the hardlink");
    assert_eq!((got("level.dat").uid(), got("level.dat").gid()), (1_000_123, 1_000_123));
    assert!(got("region.mca").blocks() * 512 < 1 << 20, "the holes kept");
    let file = to.join("level.dat");
    assert_eq!(xattr(&file, "user.korpis"), b"kept", "the extended attribute");
    assert_eq!(xattr(&file, "system.posix_acl_access"), acl(), "the ACL");

    std::fs::remove_file(from.join("level.bak")).unwrap();
    std::fs::write(from.join("new"), "since").unwrap();
    let (_, received) = pass(&source, &destination);
    let TreeEnd::Done(changed) = received else { panic!("{received:?}") };
    assert!(changed < whole, "only what differs: {changed} of {whole}");
    assert!(!to.join("level.bak").exists(), "deleted where the source deleted it");
    assert_eq!(std::fs::read_to_string(to.join("new")).unwrap(), "since");
}
