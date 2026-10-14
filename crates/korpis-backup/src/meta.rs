// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a backup records of an inode, and how it is read: the kind, the permission bits, the
//! owner, the times, and the extended attributes, POSIX ACLs among them. Everything is read
//! through a descriptor already open on the inode, or with `AT_SYMLINK_NOFOLLOW` relative to its
//! directory, so a link swapped in by the workload while its volume is read is recorded as a
//! link, never followed.

use korpis_proto::korpis::backup::v1 as pb;
use rustix::fs::{FileType, Stat};
use std::os::fd::BorrowedFd;
use std::os::unix::ffi::OsStrExt as _;

/// A number as `u64`, whatever width the platform's `stat` gives it.
pub(crate) fn wide<T: TryInto<u64>>(v: T) -> u64 {
    v.try_into().unwrap_or(u64::MAX)
}

fn narrow<T: TryInto<u32>>(v: T) -> u32 {
    v.try_into().unwrap_or(u32::MAX)
}

fn signed<T: TryInto<i64>>(v: T) -> i64 {
    v.try_into().unwrap_or(i64::MAX)
}

/// The kind of inode `st` describes.
pub(crate) fn kind_of(st: &Stat) -> pb::EntryKind {
    match FileType::from_raw_mode(st.st_mode) {
        FileType::RegularFile => pb::EntryKind::ENTRY_KIND_FILE,
        FileType::Directory => pb::EntryKind::ENTRY_KIND_DIRECTORY,
        FileType::Symlink => pb::EntryKind::ENTRY_KIND_SYMLINK,
        FileType::Fifo => pb::EntryKind::ENTRY_KIND_FIFO,
        FileType::CharacterDevice => pb::EntryKind::ENTRY_KIND_CHAR_DEVICE,
        FileType::BlockDevice => pb::EntryKind::ENTRY_KIND_BLOCK_DEVICE,
        FileType::Socket => pb::EntryKind::ENTRY_KIND_SOCKET,
        FileType::Unknown => pb::EntryKind::ENTRY_KIND_UNSPECIFIED,
    }
}

/// The entry for the inode `st` describes, named `name`, without its data or attributes.
pub(crate) fn entry_of(name: &[u8], st: &Stat) -> pb::Entry {
    let file = kind_of(st) == pb::EntryKind::ENTRY_KIND_FILE;
    let device = matches!(
        kind_of(st),
        pb::EntryKind::ENTRY_KIND_CHAR_DEVICE | pb::EntryKind::ENTRY_KIND_BLOCK_DEVICE
    );
    pb::Entry {
        name: name.to_vec(),
        kind: kind_of(st).into(),
        mode: st.st_mode & 0o7777,
        uid: st.st_uid,
        gid: st.st_gid,
        mtime_seconds: signed(st.st_mtime),
        mtime_nanos: narrow(st.st_mtime_nsec),
        ctime_seconds: signed(st.st_ctime),
        ctime_nanos: narrow(st.st_ctime_nsec),
        size: if file { wide(st.st_size) } else { 0 },
        rdev: if device { wide(st.st_rdev) } else { 0 },
        ino: wide(st.st_ino),
        ..Default::default()
    }
}

/// The bytes of a call that answers how much it needs when given nothing.
fn sized(
    mut call: impl FnMut(&mut [u8]) -> rustix::io::Result<usize>,
) -> rustix::io::Result<Vec<u8>> {
    // An attribute may grow between the two calls; a few attempts settle it.
    for _ in 0..4 {
        let need = call(&mut [])?;
        let mut buf = vec![0u8; need];
        match call(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                return Ok(buf);
            }
            Err(rustix::io::Errno::RANGE) => {}
            Err(e) => return Err(e),
        }
    }
    Err(rustix::io::Errno::RANGE)
}

/// The extended attributes of the inode `fd` is open on, by name. A filesystem without them has
/// none; one removed while it is read is not recorded.
pub(crate) fn xattrs(fd: BorrowedFd<'_>) -> rustix::io::Result<Vec<pb::Xattr>> {
    let names = match sized(|b| rustix::fs::flistxattr(fd, b)) {
        Ok(n) => n,
        Err(rustix::io::Errno::NOTSUP) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for name in names.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let os = std::ffi::OsStr::from_bytes(name);
        match sized(|b| rustix::fs::fgetxattr(fd, os, b)) {
            Ok(value) => out.push(pb::Xattr { name: name.to_vec(), value, ..Default::default() }),
            Err(rustix::io::Errno::NODATA) => {}
            Err(e) => return Err(e),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}
