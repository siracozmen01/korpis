// SPDX-License-Identifier: AGPL-3.0-or-later

//! One layer of an OCI image applied to the tree its lower layers made, as the image layer
//! specification says: an entry replaces whatever was at its path, `.wh.NAME` removes the `NAME` a
//! lower layer made, and `.wh..wh..opq` removes what the lower layers put in its directory. Files,
//! directories, symlinks, hard links, and FIFOs keep the owner and mode the layer records, which
//! must be one of the workload's 65,536 ids; a device is skipped and counted, since the guest's
//! `/dev` is the kernel's own (§6 of docs/04-runtimes.md). Every path is resolved beneath the tree
//! ([`crate::tree`]).

use crate::tree;
use rustix::fs::{
    AtFlags, FileType, Gid, Mode, OFlags, Timespec, Timestamps, Uid, chownat, fchmod, fchown,
    futimens, linkat, mknodat, openat, symlinkat,
};
use std::collections::HashSet;
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

/// The most ids a workload has.
const IDS: u64 = 65_536;

/// What applying a layer did beyond what it holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Applied {
    /// Device nodes it held, which were not made.
    pub devices_skipped: u64,
}

fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

/// A numeric field of a header, `raw`, read by `read` unless it holds nothing but NULs and spaces,
/// when it is zero, as Go's `archive/tar` reads it: containerd unpacks a container's layers with
/// that, so a layer the container tier runs is one a guest's drive is built from too.
fn numeric(raw: &[u8], read: impl FnOnce() -> io::Result<u64>) -> io::Result<u64> {
    if raw.iter().all(|b| *b == 0 || *b == b' ') { Ok(0) } else { read() }
}

/// The owner a header records, refused unless it is one of the workload's ids.
fn owner(header: &tar::Header) -> io::Result<(Uid, Gid)> {
    let old = header.as_old();
    let uid = numeric(&old.uid, || header.uid())?;
    let gid = numeric(&old.gid, || header.gid())?;
    if uid >= IDS || gid >= IDS {
        return Err(invalid(format!("an entry owned by {uid}:{gid}, outside the 65536 ids")));
    }
    let id = |n: u64| u32::try_from(n).map_err(|_| invalid("an id out of range"));
    Ok((Uid::from_raw(id(uid)?), Gid::from_raw(id(gid)?)))
}

fn mode(header: &tar::Header) -> io::Result<Mode> {
    let mode = numeric(&header.as_old().mode, || header.mode().map(u64::from))?;
    Ok(Mode::from_raw_mode(u32::try_from(mode & 0o7777).unwrap_or(0)))
}

/// A file's contents, owner, mode, and modification time, at `name` in `dir`.
fn file(dir: &OwnedFd, name: &Path, entry: &mut tar::Entry<'_, impl Read>) -> io::Result<()> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let made = openat(dir, name, flags, Mode::from_raw_mode(0o600))?;
    let mut out = std::fs::File::from(made);
    io::copy(entry, &mut out)?;
    let header = entry.header();
    let (uid, gid) = owner(header)?;
    // The owner first: changing it clears the set-id bits, which the mode then sets.
    fchown(&out, Some(uid), Some(gid))?;
    fchmod(&out, mode(header)?)?;
    let mtime = numeric(&header.as_old().mtime, || header.mtime())?;
    let when = Timespec { tv_sec: i64::try_from(mtime).unwrap_or(0), tv_nsec: 0 };
    futimens(&out, &Timestamps { last_access: when, last_modification: when })?;
    Ok(())
}

/// What the entry at `rel` is made as.
fn entry(root: &OwnedFd, rel: &Path, e: &mut tar::Entry<'_, impl Read>) -> io::Result<bool> {
    let kind = e.header().entry_type();
    if kind.is_dir() {
        let made = tree::dir(root, rel);
        let made = match made {
            Err(_) if !rel.as_os_str().is_empty() => {
                let (parent, name) = tree::parent(root, rel)?;
                tree::remove(&parent, &name)?;
                tree::dir(root, rel)?
            }
            other => other?,
        };
        let (uid, gid) = owner(e.header())?;
        fchown(&made, Some(uid), Some(gid))?;
        fchmod(&made, mode(e.header())?)?;
        return Ok(true);
    }
    if kind.is_character_special() || kind.is_block_special() {
        return Ok(false);
    }
    let (parent, name) = tree::parent(root, rel)?;
    tree::remove(&parent, &name)?;
    if kind.is_file() || kind == tar::EntryType::Continuous {
        file(&parent, &name, e)?;
    } else if kind.is_symlink() {
        let target = e.link_name()?.ok_or_else(|| invalid("a symlink to nothing"))?;
        symlinkat(target.as_ref(), &parent, &name)?;
        let (uid, gid) = owner(e.header())?;
        chownat(&parent, &name, Some(uid), Some(gid), AtFlags::SYMLINK_NOFOLLOW)?;
    } else if kind.is_hard_link() {
        let target = e.link_name()?.ok_or_else(|| invalid("a hard link to nothing"))?;
        let (from, from_name) = tree::parent(root, &tree::normal(&target)?)?;
        linkat(&from, &from_name, &parent, &name, AtFlags::empty())?;
    } else if kind.is_fifo() {
        mknodat(&parent, &name, FileType::Fifo, mode(e.header())?, 0)?;
        let (uid, gid) = owner(e.header())?;
        chownat(&parent, &name, Some(uid), Some(gid), AtFlags::SYMLINK_NOFOLLOW)?;
    } else {
        return Err(invalid(format!("{}: an entry of kind {kind:?}", rel.display())));
    }
    Ok(true)
}

/// Applies the layer `archive`, uncompressed, to the tree at `root`.
pub fn apply(root: &OwnedFd, archive: impl Read) -> io::Result<Applied> {
    let mut archive = tar::Archive::new(archive);
    let mut made: HashSet<PathBuf> = HashSet::new();
    let mut applied = Applied::default();
    for e in archive.entries()? {
        let mut e = e?;
        let rel = tree::normal(&e.path()?)?;
        let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let above = rel.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        if name == ".wh..wh..opq" {
            let dir = tree::dir(root, &above)?;
            tree::empty(&dir, |child| made.contains(&above.join(child)))?;
        } else if let Some(hidden) = name.strip_prefix(".wh.") {
            // A whiteout hides what lower layers made, never what its own layer did.
            if !made.contains(&above.join(hidden)) {
                tree::remove(&tree::dir(root, &above)?, Path::new(hidden))?;
            }
        } else if entry(root, &rel, &mut e)? {
            made.insert(rel);
        } else {
            applied.devices_skipped += 1;
        }
    }
    Ok(applied)
}

#[cfg(test)]
mod tests;
