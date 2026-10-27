// SPDX-License-Identifier: AGPL-3.0-or-later

//! A shared storage class (§2.1 of docs/06-storage.md): a directory where the operator mounted a
//! filesystem reached over the network, which other nodes mount too. The helper mounts nothing.
//! It refuses the class unless the filesystem is one reached over the network, since a local one
//! named shared would let a workload placed on another node find none of its data, and unless
//! root may make a directory there, give it to another owner, and remove it, which every volume
//! needs and an export squashing root forbids. Which storage it is, it reads from a marker at the
//! root that the first helper on it linked, so that the control plane can tell which nodes share
//! one store by what each read at its own mount.

use rustix::fs::{AtFlags, Gid, Mode, OFlags, Uid};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::path::Path;

/// `statfs` magic numbers of filesystems reached over the network.
const NFS_MAGIC: u64 = 0x6969;
const CEPH_MAGIC: u64 = 0x00C3_6400;
const SMB2_MAGIC: u64 = 0xFE53_4D42;
const CIFS_MAGIC: u64 = 0xFF53_4D42;
const FUSE_MAGIC: u64 = 0x6573_5546;

/// The FUSE filesystems reached over the network, by their type in the mount table.
const FUSE_NETWORK: [&str; 2] = ["fuse.ceph-fuse", "fuse.glusterfs"];

/// The marker naming the store, at the class's root.
const MARKER: &str = ".korpis-store";

/// A new random identifier, in the hyphenated form of a version 4 UUID.
fn new_id() -> io::Result<String> {
    let mut b = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex = b.iter().fold(String::new(), |mut out, x| {
        let _ = write!(out, "{x:02x}");
        out
    });
    let part = |r: std::ops::Range<usize>| hex.get(r).unwrap_or_default().to_owned();
    Ok([part(0..8), part(8..12), part(12..16), part(16..20), part(20..32)].join("-"))
}

/// Whether `s` is a UUID in hyphenated lower-case form.
fn is_id(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

/// What the helper requires of a shared class's filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// It must be reached over the network, as every operator's is.
    Required,
    /// Whatever it is, taken as shared: for tests, which have two nodes on one machine share
    /// a directory, and no network filesystem.
    #[cfg(feature = "testing")]
    Assumed,
}

/// Why a directory is not a shared class.
fn refused(why: String) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, why)
}

/// Whether the filesystem at `root` is reached over the network, or what it is instead.
fn network_filesystem(root: &File, path: &Path) -> Result<(), String> {
    let stat = rustix::fs::fstatfs(root).map_err(|e| format!("statfs: {e}"))?;
    let magic = u64::try_from(stat.f_type).unwrap_or(0);
    match magic {
        NFS_MAGIC | CEPH_MAGIC | SMB2_MAGIC | CIFS_MAGIC => Ok(()),
        FUSE_MAGIC => match fuse_type(path) {
            Some(t) if FUSE_NETWORK.contains(&t.as_str()) => Ok(()),
            Some(t) => Err(format!("{t} is not a filesystem reached over the network")),
            None => Err("a FUSE filesystem of no type the mount table names".into()),
        },
        other => Err(format!("filesystem {other:#x} is not one reached over the network")),
    }
}

/// The type the mount table gives the mount holding `path`: the longest mount point it is under.
fn fuse_type(path: &Path) -> Option<String> {
    let table = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let mut best: Option<(usize, String)> = None;
    for line in table.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let point = fields.get(4)?;
        let after = fields.iter().position(|f| *f == "-")?;
        let kind = fields.get(after + 1)?;
        if path.starts_with(point) && best.as_ref().is_none_or(|(len, _)| point.len() > *len) {
            best = Some((point.len(), (*kind).to_owned()));
        }
    }
    best.map(|(_, kind)| kind)
}

/// Opening a name under a held directory as it is, never through a link: another client of the
/// export may put a link in place of anything this helper made there (Finding 202 of
/// docs/23-walkthroughs.md).
const NOFOLLOW: OFlags = OFlags::NOFOLLOW.union(OFlags::CLOEXEC);

/// That root may make a directory in `root`, give it away, and remove it.
fn probe(root: &File) -> Result<(), String> {
    let name = format!(".korpis-probe-{}", new_id().map_err(|e| e.to_string())?);
    let squashed =
        |e: rustix::io::Errno| format!("{}: is root squashed on this export?", io::Error::from(e));
    rustix::fs::mkdirat(root, &name, Mode::from_raw_mode(0o700)).map_err(squashed)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | NOFOLLOW;
    let nobody = (Uid::from_raw_unchecked(65_534), Gid::from_raw_unchecked(65_534));
    let given = rustix::fs::openat(root, &name, flags, Mode::empty())
        .and_then(|made| rustix::fs::fchown(&made, Some(nobody.0), Some(nobody.1)))
        .map_err(squashed);
    let removed = rustix::fs::unlinkat(root, &name, AtFlags::REMOVEDIR).map_err(squashed);
    given.and(removed)
}

/// The store at `root`: the identifier the first helper linked as its marker, which this one
/// writes when it is the first. A file of its own name is linked to the marker's, which succeeds
/// for one writer alone on NFS as locally, and a link count of two on its own file is success
/// whose answer was lost, as `open(2)` says of NFS.
fn store_at(root: &File) -> io::Result<String> {
    if let Some(store) = read_marker(root)? {
        return Ok(store);
    }
    let mine = new_id()?;
    let own = format!("{MARKER}.{mine}");
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | NOFOLLOW;
    let mut file = File::from(rustix::fs::openat(root, &own, flags, Mode::from_raw_mode(0o644))?);
    file.write_all(mine.as_bytes())?;
    file.sync_all()?;
    let linked = rustix::fs::linkat(root, &own, root, MARKER, AtFlags::empty());
    let count = rustix::fs::statat(root, &own, AtFlags::SYMLINK_NOFOLLOW).map(|s| s.st_nlink);
    let won = linked.is_ok() || count.is_ok_and(|n| n == 2);
    rustix::fs::unlinkat(root, &own, AtFlags::empty())?;
    if won {
        return Ok(mine);
    }
    read_marker(root)?.ok_or_else(|| refused(format!("{MARKER} is unreadable")))
}

/// The store the marker in `root` names, if there is one; a marker that names none, or is a
/// link, is refused.
fn read_marker(root: &File) -> io::Result<Option<String>> {
    let mut text = String::new();
    match rustix::fs::openat(root, MARKER, OFlags::RDONLY | NOFOLLOW, Mode::empty()) {
        Ok(f) => File::from(f).take(64).read_to_string(&mut text)?,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let store = text.trim().to_ascii_lowercase();
    if !is_id(&store) {
        return Err(refused(format!("{MARKER} names no store")));
    }
    Ok(Some(store))
}

/// The store a shared class at `root`, open as `handle`, is, once the directory is found to be
/// one: reached over the network where `network` requires it, and where root may make, give
/// away, and remove a directory.
pub(super) fn open(root: &Path, handle: &File, network: Network) -> io::Result<String> {
    let reached = match network {
        Network::Required => network_filesystem(handle, root),
        #[cfg(feature = "testing")]
        Network::Assumed => Ok(()),
    };
    reached.map_err(|why| refused(format!("{} is not shared: {why}", root.display())))?;
    probe(handle).map_err(|why| refused(format!("{}: {why}", root.display())))?;
    store_at(handle).map_err(|e| refused(format!("{}: {e}", root.display())))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]
    use super::*;

    #[test]
    fn the_first_helper_names_the_store_and_the_next_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = File::open(dir.path()).unwrap();
        let first = store_at(&root).unwrap();
        assert_eq!(store_at(&root).unwrap(), first, "read, not written again");
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "its own file is removed once linked");
        std::fs::write(dir.path().join(MARKER), "not a store").unwrap();
        assert!(store_at(&root).is_err(), "a marker naming no store is refused");
        assert!(is_id(&first) && is_id(&new_id().unwrap()));
        assert!(!is_id("0b9c1f40-3d2e-4c55-9f0a-7e6d5c4b3a2g"));
    }

    #[test]
    fn a_marker_that_is_a_link_is_refused_and_not_followed() {
        let (dir, elsewhere) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let target = elsewhere.path().join("id");
        std::fs::write(&target, new_id().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(MARKER)).unwrap();
        let root = File::open(dir.path()).unwrap();
        assert!(store_at(&root).is_err(), "another client of the export may have put it there");
    }

    #[test]
    fn a_local_directory_is_not_shared() {
        let dir = tempfile::tempdir().unwrap();
        let handle = File::open(dir.path()).unwrap();
        let refused = open(dir.path(), &handle, Network::Required).unwrap_err();
        assert!(refused.to_string().contains("not shared"), "{refused}");
        assert!(!dir.path().join(MARKER).exists(), "and no store is named on it");
    }
}
