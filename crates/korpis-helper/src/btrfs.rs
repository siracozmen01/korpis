// SPDX-License-Identifier: AGPL-3.0-or-later

//! btrfs (§2 of docs/06-storage.md): a volume is a subvolume, and its size is a qgroup limit on
//! what the subvolume references, which the kernel checks in the write path, where exceeding it
//! is `EDQUOT`. Usage is read from the qgroup accounting in the quota tree, never computed.
//! btrfs has no inode quota, so a class on it limits bytes alone, and says so.
//!
//! Qgroup accounting can become inconsistent, and while it is, limits are not enforced: the
//! kernel counts nothing. A class reads the quota tree's status when it opens and enforces only
//! if quotas are on and consistent; a usage read says it is not accounted while they are not. The
//! helper never destroys a qgroup: on Linux 6.8, destroying a deleted subvolume's stale qgroup
//! marked the accounting inconsistent at the next commit, which lifted every limit on the
//! filesystem until a rescan (research/evidence.md). A deleted volume's empty qgroup is left for
//! an operator's `btrfs qgroup clear-stale`.
//!
//! This is one of the crate's two unsafe modules, with [`crate::quota`]: the btrfs ioctls have no
//! safe wrapper in the libraries the helper may link. Each call passes a descriptor borrowed for
//! its duration and a pointer to one correctly laid out structure owned by the caller.

#![expect(unsafe_code, reason = "the btrfs ioctls have no safe wrapper")]

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd as _;

/// The `statfs` magic number of btrfs.
pub const MAGIC: u64 = 0x9123_683E;
/// The inode number of every subvolume's root directory (`BTRFS_FIRST_FREE_OBJECTID`).
pub const SUBVOLUME_INODE: u64 = 256;

/// `_IOC(dir, 0x94, nr, size)` in the generic encoding amd64 and arm64 share.
const fn ioc(dir: u64, nr: u64, size: u64) -> libc::c_ulong {
    (dir << 30) | (size << 16) | (0x94 << 8) | nr
}
const WRITE: u64 = 1;
const READ: u64 = 2;
const IOC_TREE_SEARCH: libc::c_ulong = ioc(READ | WRITE, 17, 4096);
const IOC_INO_LOOKUP: libc::c_ulong = ioc(READ | WRITE, 18, 4096);
const IOC_SUBVOL_CREATE_V2: libc::c_ulong = ioc(WRITE, 24, 4096);
const IOC_QGROUP_LIMIT: libc::c_ulong = ioc(READ, 43, 48);
const IOC_SNAP_DESTROY_V2: libc::c_ulong = ioc(WRITE, 63, 4096);

/// The quota tree, and its item types (`linux/btrfs_tree.h`).
const QUOTA_TREE: u64 = 8;
const QGROUP_STATUS: u32 = 240;
const QGROUP_INFO: u32 = 242;
const QGROUP_LIMIT: u32 = 244;
/// The status flags.
const STATUS_ON: u64 = 1 << 0;
const STATUS_INCONSISTENT: u64 = 1 << 2;
/// A limit on referenced bytes, and the value that clears it.
const LIMIT_MAX_RFER: u64 = 1 << 0;
const CLEAR: u64 = u64::MAX;

/// The size of `struct btrfs_ioctl_search_args`, and of its key.
const SEARCH_ARGS: usize = 4096;
const SEARCH_KEY: usize = 104;
/// The size of `struct btrfs_ioctl_search_header`.
const SEARCH_HEADER: usize = 32;
/// The longest subvolume name (`BTRFS_SUBVOL_NAME_MAX`), and the name's offset in
/// `struct btrfs_ioctl_vol_args_v2`.
const NAME_MAX: usize = 4039;
const NAME_AT: usize = 56;

/// Runs `request` on `fd` with the bytes of `args`, which are its structure.
fn ioctl(fd: &File, request: libc::c_ulong, args: &mut [u8]) -> io::Result<()> {
    let ptr = args.as_mut_ptr();
    // SAFETY: `fd` is open for the whole call, because it is borrowed for it. `ptr` points to
    // `args`, exclusively borrowed for the call, and every caller passes a buffer exactly the
    // size the request number encodes, laid out as the structure the request reads and writes:
    // the kernel touches no byte outside it.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), request, ptr) };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

fn put(buf: &mut [u8], at: usize, value: u64) {
    if let Some(slot) = buf.get_mut(at..at + 8) {
        slot.copy_from_slice(&value.to_ne_bytes());
    }
}

fn put32(buf: &mut [u8], at: usize, value: u32) {
    if let Some(slot) = buf.get_mut(at..at + 4) {
        slot.copy_from_slice(&value.to_ne_bytes());
    }
}

fn get(buf: &[u8], at: usize) -> u64 {
    buf.get(at..at + 8).and_then(|b| b.try_into().ok()).map_or(0, u64::from_ne_bytes)
}

fn get32(buf: &[u8], at: usize) -> u32 {
    buf.get(at..at + 4).and_then(|b| b.try_into().ok()).map_or(0, u32::from_ne_bytes)
}

/// A little-endian field of an on-disk item.
fn le(item: &[u8], at: usize) -> u64 {
    item.get(at..at + 8).and_then(|b| b.try_into().ok()).map_or(0, u64::from_le_bytes)
}

/// The vol args naming `name`, or an error if it cannot be one.
fn named(name: &str) -> io::Result<Vec<u8>> {
    if name.is_empty() || name.len() > NAME_MAX || name.contains(['/', '\0']) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a subvolume name"));
    }
    let mut args = vec![0u8; 4096];
    if let Some(slot) = args.get_mut(NAME_AT..NAME_AT + name.len()) {
        slot.copy_from_slice(name.as_bytes());
    }
    Ok(args)
}

/// Creates subvolume `name` in the directory `parent`.
pub fn create_subvolume(parent: &File, name: &str) -> io::Result<()> {
    ioctl(parent, IOC_SUBVOL_CREATE_V2, &mut named(name)?)
}

/// Deletes subvolume `name` in the directory `parent`, and everything in it.
pub fn destroy_subvolume(parent: &File, name: &str) -> io::Result<()> {
    ioctl(parent, IOC_SNAP_DESTROY_V2, &mut named(name)?)
}

/// The identifier of the subvolume `subvolume` is the root of, which is its qgroup's.
pub fn subvolume_id(subvolume: &File) -> io::Result<u64> {
    let mut args = vec![0u8; 4096];
    put(&mut args, 8, SUBVOLUME_INODE);
    ioctl(subvolume, IOC_INO_LOOKUP, &mut args)?;
    Ok(get(&args, 0))
}

/// Limits what `subvolume` references to `bytes`; zero lifts the limit.
pub fn limit(subvolume: &File, bytes: u64) -> io::Result<()> {
    let mut args = [0u8; 48];
    // qgroupid zero is the subvolume the descriptor is in.
    put(&mut args, 8, LIMIT_MAX_RFER);
    put(&mut args, 16, if bytes == 0 { CLEAR } else { bytes });
    ioctl(subvolume, IOC_QGROUP_LIMIT, &mut args)
}

/// The quota tree's items of `kind` at `offset` (every offset when `None`): each item's offset
/// and bytes. No quota tree is an empty answer. The search key is `struct
/// btrfs_ioctl_search_key`: the tree at 0, the objectid range at 8 and 16 (zero for every qgroup
/// item), the offset range at 24 and 32, the transid range at 40 and 48, the type range at 56
/// and 60, and how many items at 64, which the kernel overwrites with how many it found. Each
/// item follows a 32-byte header whose offset is at 16 and whose length is at 28.
fn quota_items(root: &File, kind: u32, offset: Option<u64>) -> io::Result<Vec<(u64, Vec<u8>)>> {
    let mut args = vec![0u8; SEARCH_ARGS];
    put(&mut args, 0, QUOTA_TREE);
    let (from, to) = offset.map_or((0, u64::MAX), |o| (o, o));
    put(&mut args, 24, from);
    put(&mut args, 32, to);
    put(&mut args, 48, u64::MAX);
    put32(&mut args, 56, kind);
    put32(&mut args, 60, kind);
    put32(&mut args, 64, 4096);
    match ioctl(root, IOC_TREE_SEARCH, &mut args) {
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(Vec::new()),
        r => r?,
    }
    let count = get32(&args, 64);
    let mut out = Vec::new();
    let mut at = SEARCH_KEY;
    for _ in 0..count {
        let item_offset = get(&args, at + 16);
        let len = usize::try_from(get32(&args, at + 28)).unwrap_or(0);
        let body = at + SEARCH_HEADER;
        let Some(item) = args.get(body..body + len) else { break };
        out.push((item_offset, item.to_vec()));
        at = body + len;
    }
    Ok(out)
}

/// Whether qgroups are on for the filesystem holding `root` and their accounting is consistent:
/// the condition under which a limit is enforced.
pub fn quota_enforcing(root: &File) -> io::Result<bool> {
    let status = quota_items(root, QGROUP_STATUS, Some(0))?;
    Ok(status.first().is_some_and(|(_, item)| {
        let flags = le(item, 16);
        flags & STATUS_ON != 0 && flags & STATUS_INCONSISTENT == 0
    }))
}

/// What subvolume `id` references, and its limit (zero for none); `None` while the accounting is
/// not to be believed.
pub fn usage(root: &File, id: u64) -> io::Result<Option<(u64, u64)>> {
    if !quota_enforcing(root)? {
        return Ok(None);
    }
    let referenced = quota_items(root, QGROUP_INFO, Some(id))?.first().map_or(0, |(_, i)| le(i, 8));
    let limit = quota_items(root, QGROUP_LIMIT, Some(id))?
        .first()
        .filter(|(_, i)| le(i, 0) & LIMIT_MAX_RFER != 0)
        .map_or(0, |(_, i)| le(i, 8));
    Ok(Some((referenced, limit)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_numbers_are_the_kernels() {
        assert_eq!(IOC_SUBVOL_CREATE_V2, 0x5000_9418);
        assert_eq!(IOC_SNAP_DESTROY_V2, 0x5000_943F);
        assert_eq!(IOC_QGROUP_LIMIT, 0x8030_942B);
        assert_eq!(IOC_TREE_SEARCH, 0xD000_9411);
        assert_eq!(IOC_INO_LOOKUP, 0xD000_9412);
    }

    #[test]
    fn a_name_is_refused_unless_it_is_one_path_component() {
        assert!(named("wl_1.data").is_ok());
        assert!(named("a/b").is_err() && named("").is_err() && named(&"x".repeat(4040)).is_err());
    }
}
