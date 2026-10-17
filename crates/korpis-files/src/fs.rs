// SPDX-License-Identifier: AGPL-3.0-or-later

//! The verbs of a file session (§3.2 of docs/06-storage.md), served beneath one root directory:
//! every path is resolved from the root's descriptor with `openat2`, never stepping above it and
//! following no link of any kind, so a symbolic link a tenant planted, or `..`, reaches nothing
//! outside. The worker's root is the workload's volumes and nothing else, so this is the second
//! of two walls, the first being that nothing else is mounted there at all.

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags, openat2, statat};
use rustix::io::Errno;
use std::os::fd::OwnedFd;

/// The most bytes one read or write moves.
pub const CHUNK: usize = 1 << 20;
/// The most entries one listing answers.
pub const PAGE: usize = 1000;
/// The deepest a recursive remove goes; a tree deeper than this is refused, not walked.
pub const DEPTH: usize = 128;

const WALK: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS)
    .union(ResolveFlags::NO_XDEV);

/// Why a verb was not done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    NotFound,
    Exists,
    Full,
    NotADirectory,
    IsADirectory,
    NotEmpty,
    Outside,
    Refused,
    Io,
}

impl From<Errno> for Fault {
    fn from(e: Errno) -> Self {
        match e {
            Errno::NOENT => Self::NotFound,
            Errno::EXIST => Self::Exists,
            Errno::DQUOT | Errno::NOSPC => Self::Full,
            Errno::NOTDIR => Self::NotADirectory,
            Errno::ISDIR => Self::IsADirectory,
            Errno::NOTEMPTY => Self::NotEmpty,
            // A link refused by the walk, or a step out of the root or across a mount.
            Errno::LOOP | Errno::XDEV | Errno::PERM | Errno::ACCESS => Self::Outside,
            // A FIFO with no reader, opened to write without waiting, or a device: neither is a
            // file a session serves.
            Errno::NXIO => Self::Refused,
            _ => Self::Io,
        }
    }
}

/// What a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    Symlink,
    Other,
}

/// A directory entry, or a path's metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    /// Seconds since the epoch.
    pub modified: i64,
}

/// A root the verbs are served beneath.
#[derive(Debug)]
pub struct Root {
    fd: OwnedFd,
    write: bool,
}

/// `path` as the walk takes it: relative to the root, empty for the root itself.
fn relative(path: &str) -> &str {
    let p = path.trim_start_matches('/');
    if p.is_empty() { "." } else { p }
}

fn kind(t: FileType) -> Kind {
    match t {
        FileType::RegularFile => Kind::File,
        FileType::Directory => Kind::Directory,
        FileType::Symlink => Kind::Symlink,
        _ => Kind::Other,
    }
}

/// `rel` beneath `at`, opened with `flags`, no link followed.
fn open_in(at: &OwnedFd, rel: &str, flags: OFlags, mode: u32) -> Result<OwnedFd, Fault> {
    let flags = flags | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    // A mode is given only with O_CREAT; openat2 refuses one otherwise.
    let mode = if flags.contains(OFlags::CREATE) { mode } else { 0 };
    Ok(openat2(at, rel, flags, Mode::from_raw_mode(mode), WALK)?)
}

/// `fd` if it is a regular file: a FIFO, a socket, or a device is neither read nor written by a
/// file session.
fn regular(fd: OwnedFd) -> Result<OwnedFd, Fault> {
    match FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode) {
        FileType::RegularFile => Ok(fd),
        FileType::Directory => Err(Fault::IsADirectory),
        _ => Err(Fault::Refused),
    }
}

/// Gives `made` to the owner of `parent`, the directory it was made in: the session writes as
/// the workload's root, and a workload running as another of its users could otherwise not
/// change what was uploaded for it.
fn owned_as(parent: &OwnedFd, made: &OwnedFd) -> Result<(), Fault> {
    let s = rustix::fs::fstat(parent)?;
    let (uid, gid) = (rustix::fs::Uid::from_raw(s.st_uid), rustix::fs::Gid::from_raw(s.st_gid));
    Ok(rustix::fs::fchown(made, Some(uid), Some(gid))?)
}

/// The parent directory of `path` beneath `root`, and the last component.
fn split(path: &str) -> Result<(&str, &str), Fault> {
    let p = relative(path);
    let (dir, name) = p.rsplit_once('/').unwrap_or((".", p));
    if name.is_empty() || name == "." || name == ".." {
        return Err(Fault::Refused);
    }
    Ok((if dir.is_empty() { "." } else { dir }, name))
}

impl Root {
    /// The directory `fd`, served for reading, and for writing when `write`.
    #[must_use]
    pub const fn new(fd: OwnedFd, write: bool) -> Self {
        Self { fd, write }
    }

    fn open(&self, path: &str, flags: OFlags, mode: u32) -> Result<OwnedFd, Fault> {
        open_in(&self.fd, relative(path), flags, mode)
    }

    fn dir(&self, path: &str) -> Result<OwnedFd, Fault> {
        self.open(path, OFlags::RDONLY | OFlags::DIRECTORY, 0)
    }

    fn writable(&self) -> Result<(), Fault> {
        if self.write { Ok(()) } else { Err(Fault::Refused) }
    }

    /// The metadata of `path`.
    pub fn stat(&self, path: &str) -> Result<Entry, Fault> {
        let (dir, name) = if relative(path) == "." { (".", ".") } else { split(path)? };
        let at = self.dir(dir)?;
        let s = statat(&at, name, AtFlags::SYMLINK_NOFOLLOW)?;
        Ok(Entry {
            name: name.to_owned(),
            kind: kind(FileType::from_raw_mode(s.st_mode)),
            size: u64::try_from(s.st_size).unwrap_or(0),
            modified: s.st_mtime,
        })
    }

    /// The entries of directory `path` after `after`, in name order, at most `limit`, and the
    /// name the next page starts after, if there is one.
    pub fn list(
        &self,
        path: &str,
        after: &str,
        limit: usize,
    ) -> Result<(Vec<Entry>, String), Fault> {
        let at = self.dir(path)?;
        let limit = limit.clamp(1, PAGE);
        // The smallest names after `after`, never more than a page and one held at once, so a
        // directory of millions of entries costs a page of memory.
        let mut smallest = std::collections::BTreeSet::new();
        for e in Dir::read_from(&at)?.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == "." || name == ".." || name.as_str() <= after {
                continue;
            }
            smallest.insert(name);
            if smallest.len() > limit + 1 {
                smallest.pop_last();
            }
        }
        let more = smallest.len() > limit;
        let names: Vec<String> = smallest.into_iter().take(limit).collect();
        let mut entries = Vec::with_capacity(names.len());
        for name in &names {
            let s = statat(&at, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)?;
            entries.push(Entry {
                name: name.clone(),
                kind: kind(FileType::from_raw_mode(s.st_mode)),
                size: u64::try_from(s.st_size).unwrap_or(0),
                modified: s.st_mtime,
            });
        }
        let next = if more { names.last().cloned().unwrap_or_default() } else { String::new() };
        Ok((entries, next))
    }

    /// `path` opened with `flags` as a regular file: never blocking on a FIFO a tenant made, and
    /// refusing a FIFO, a socket, or a device, which a file session neither reads nor writes.
    fn file(&self, path: &str, flags: OFlags) -> Result<OwnedFd, Fault> {
        regular(self.open(path, flags | OFlags::NONBLOCK, 0)?)
    }

    /// As [`Self::file`], made first if it is not there, and then given to the owner of the
    /// directory it is in.
    fn created(&self, path: &str, flags: OFlags) -> Result<OwnedFd, Fault> {
        let (dir, name) = split(path)?;
        let at = self.dir(dir)?;
        let flags = flags | OFlags::NONBLOCK;
        match open_in(&at, name, flags | OFlags::CREATE | OFlags::EXCL, 0o644) {
            Ok(fd) => {
                owned_as(&at, &fd)?;
                Ok(fd)
            }
            Err(Fault::Exists) => regular(open_in(&at, name, flags, 0)?),
            Err(e) => Err(e),
        }
    }

    /// At most `length` bytes of `path` from `offset`, never more than a chunk.
    pub fn read(&self, path: &str, offset: u64, length: usize) -> Result<Vec<u8>, Fault> {
        let fd = self.file(path, OFlags::RDONLY)?;
        let mut buf = vec![0u8; length.min(CHUNK)];
        let n = rustix::io::pread(&fd, &mut buf, offset)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Writes `data` to `path` at `offset`; how many bytes.
    pub fn write(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
        create: bool,
        truncate: bool,
    ) -> Result<u64, Fault> {
        self.writable()?;
        if data.len() > CHUNK || (truncate && offset != 0) {
            return Err(Fault::Refused);
        }
        let flags = if truncate { OFlags::WRONLY | OFlags::TRUNC } else { OFlags::WRONLY };
        let fd = if create { self.created(path, flags)? } else { self.file(path, flags)? };
        let mut done = 0;
        while let Some(rest) = data.get(done..).filter(|r| !r.is_empty()) {
            let at = offset + u64::try_from(done).unwrap_or(u64::MAX);
            done += rustix::io::pwrite(&fd, rest, at)?;
        }
        Ok(u64::try_from(done).unwrap_or(u64::MAX))
    }

    /// Makes directory `path`.
    pub fn mkdir(&self, path: &str) -> Result<(), Fault> {
        self.writable()?;
        let (dir, name) = split(path)?;
        let at = self.dir(dir)?;
        rustix::fs::mkdirat(&at, name, Mode::from_raw_mode(0o755))?;
        owned_as(&at, &open_in(&at, name, OFlags::RDONLY | OFlags::DIRECTORY, 0)?)
    }

    /// Renames `from` to `to`, refusing to replace anything; across volumes refused.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), Fault> {
        self.writable()?;
        let ((fd, fname), (td, tname)) = (split(from)?, split(to)?);
        let flags = rustix::fs::RenameFlags::NOREPLACE;
        Ok(rustix::fs::renameat_with(&self.dir(fd)?, fname, &self.dir(td)?, tname, flags)?)
    }

    /// Removes `path`: a file, an empty directory, or with `recursive` a directory and all in it.
    pub fn remove(&self, path: &str, recursive: bool) -> Result<u64, Fault> {
        self.writable()?;
        let (dir, name) = split(path)?;
        let at = self.dir(dir)?;
        remove_at(&at, name, recursive.then_some(DEPTH))
    }
}

/// Removes `name` in `at`, counting what went; a directory's contents first while `depth` allows
/// going down, refused below that rather than exhausting the worker's stack or descriptors.
fn remove_at(at: &OwnedFd, name: &str, depth: Option<usize>) -> Result<u64, Fault> {
    let s = statat(at, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(s.st_mode) != FileType::Directory {
        rustix::fs::unlinkat(at, name, AtFlags::empty())?;
        return Ok(1);
    }
    let mut removed = 0;
    if let Some(depth) = depth {
        let below = depth.checked_sub(1).ok_or(Fault::Refused)?;
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
        let inner = openat2(at, name, flags, Mode::empty(), WALK)?;
        let names: Vec<String> = Dir::read_from(&inner)?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "." && n != "..")
            .collect();
        for n in names {
            removed += remove_at(&inner, &n, Some(below))?;
        }
    }
    rustix::fs::unlinkat(at, name, AtFlags::REMOVEDIR)?;
    Ok(removed + 1)
}
