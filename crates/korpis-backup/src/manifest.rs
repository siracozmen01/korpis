// SPDX-License-Identifier: AGPL-3.0-or-later

//! A volume's manifest: a canonical walk of its tree, which a migration's two nodes compute over
//! what one sent and the other received, so the control plane compares what arrived and not only
//! that bytes did. Every entry is recorded depth first, each directory's names in byte order, as
//! its path, kind, permission bits, owner and group by number, and extended attributes with POSIX
//! ACLs among them, sorted by name; a regular file adds its size, modification time, the SHA-256
//! of the data it holds and the runs that hold it, so a lost hole is a difference; a symlink its
//! target; a device node its numbers; and a second path to a hard-linked inode the first path
//! instead. The digest is the SHA-256 of every record; the runs of [`RUN`] records each have their
//! first path and their own digest, so where two manifests differ is known to a run. The walk reads
//! as the backup's does: each directory through a descriptor opened relative to its parent's with
//! `O_NOFOLLOW`, never leaving the tree's filesystem, a link recorded and never followed.

use crate::error::{Error, Result, sys};
use crate::meta::{kind_of, wide, xattrs};
use crate::walk::{names, open_dir, os};
use korpis_proto::korpis::backup::v1 as pb;
use rustix::fs::{AtFlags, Mode, OFlags, Stat};
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsFd as _, OwnedFd};

/// How many records a run of the manifest holds.
pub const RUN: usize = 1024;

/// The deepest a walk goes, as a backup's.
const DEPTH: usize = 2048;

/// One run of a manifest's records: its first path and its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// The first record's path.
    pub first_path: String,
    /// The SHA-256 of the run's records, in hexadecimal.
    pub digest: String,
}

/// A volume's manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The SHA-256 of every record, in hexadecimal.
    pub digest: String,
    /// The records in runs.
    pub chunks: Vec<Chunk>,
}

/// `bytes` in lowercase hexadecimal.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A record's bytes, each field prefixed with its length where it has none of its own.
#[derive(Default)]
struct Record(Vec<u8>);

impl Record {
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(&wide(b.len()).to_be_bytes());
        self.0.extend_from_slice(b);
    }

    fn number(&mut self, n: u64) {
        self.0.extend_from_slice(&n.to_be_bytes());
    }
}

/// The walk, with the digests it has made so far.
struct Walk {
    dev: u64,
    all: Sha256,
    run: Sha256,
    in_run: usize,
    first: String,
    chunks: Vec<Chunk>,
    /// The first path of each hard-linked inode, by inode.
    linked: HashMap<u64, Vec<u8>>,
}

impl Walk {
    /// Adds the record of the entry at `path`.
    fn record(&mut self, path: &[u8], r: &Record) {
        if self.in_run == 0 {
            self.first = String::from_utf8_lossy(path).into_owned();
        }
        self.all.update(&r.0);
        self.run.update(&r.0);
        self.in_run += 1;
        if self.in_run == RUN {
            self.close_run();
        }
    }

    fn close_run(&mut self) {
        if self.in_run == 0 {
            return;
        }
        let digest = hex(&std::mem::take(&mut self.run).finalize());
        let first_path = std::mem::take(&mut self.first);
        self.chunks.push(Chunk { first_path, digest });
        self.in_run = 0;
    }

    /// What every entry records: its path, kind, permission bits, owner, and attributes.
    fn head(path: &[u8], st: &Stat, fd: Option<&OwnedFd>) -> Result<Record> {
        let mut r = Record::default();
        r.bytes(path);
        r.number(wide(kind_of(st).to_i32()));
        r.number(u64::from(st.st_mode & 0o7777));
        r.number(u64::from(st.st_uid));
        r.number(u64::from(st.st_gid));
        let attrs = match fd {
            Some(fd) => xattrs(fd.as_fd()).map_err(sys(&String::from_utf8_lossy(path)))?,
            None => Vec::new(),
        };
        r.number(wide(attrs.len()));
        for a in attrs {
            r.bytes(&a.name);
            r.bytes(&a.value);
        }
        Ok(r)
    }

    /// Records the directory `fd` is open on, at `path`, and everything under it.
    fn dir(&mut self, fd: &OwnedFd, path: &[u8], depth: usize) -> Result<()> {
        let shown = String::from_utf8_lossy(path).into_owned();
        if depth > DEPTH {
            return Err(Error::Refused(format!("{shown}: deeper than {DEPTH} directories")));
        }
        // What is read is what the descriptor is open on, whatever the name pointed to when it was
        // looked at: a tree mounted over it since is no part of the volume.
        let st = rustix::fs::fstat(fd).map_err(sys(&shown))?;
        if wide(st.st_dev) != self.dev {
            return Ok(());
        }
        let r = Self::head(path, &st, Some(fd))?;
        self.record(path, &r);
        for name in names(fd).map_err(sys(&shown))? {
            let mut at = path.to_vec();
            if !at.is_empty() {
                at.push(b'/');
            }
            at.extend_from_slice(&name);
            self.entry(fd, &name, &at, depth)?;
        }
        Ok(())
    }

    /// Records the entry `name` of the directory `dir` is open on, at `path`.
    fn entry(&mut self, dir: &OwnedFd, name: &[u8], path: &[u8], depth: usize) -> Result<()> {
        let shown = String::from_utf8_lossy(path).into_owned();
        let st = rustix::fs::statat(dir, os(name), AtFlags::SYMLINK_NOFOLLOW).map_err(sys(&shown))?;
        if wide(st.st_dev) != self.dev {
            // Another filesystem mounted in the tree is no part of the volume.
            return Ok(());
        }
        match kind_of(&st) {
            pb::EntryKind::ENTRY_KIND_DIRECTORY => {
                let fd = open_dir(dir, name).map_err(sys(&shown))?;
                self.dir(&fd, path, depth + 1)
            }
            pb::EntryKind::ENTRY_KIND_FILE => self.file(dir, name, path),
            pb::EntryKind::ENTRY_KIND_SYMLINK => {
                let mut r = Self::head(path, &st, None)?;
                let target = rustix::fs::readlinkat(dir, os(name), Vec::new()).map_err(sys(&shown))?;
                r.bytes(target.as_bytes());
                self.record(path, &r);
                Ok(())
            }
            _ => {
                let mut r = Self::head(path, &st, None)?;
                r.number(wide(st.st_rdev));
                self.record(path, &r);
                Ok(())
            }
        }
    }

    /// Records the regular file `name` of `dir`, at `path`: its data and the runs holding it, or,
    /// for a second path to a hard-linked inode, the first path.
    fn file(&mut self, dir: &OwnedFd, name: &[u8], path: &[u8]) -> Result<()> {
        let shown = String::from_utf8_lossy(path).into_owned();
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(dir, os(name), flags, Mode::empty()).map_err(sys(&shown))?;
        let st = &rustix::fs::fstat(&fd).map_err(sys(&shown))?;
        if wide(st.st_dev) != self.dev || kind_of(st) != pb::EntryKind::ENTRY_KIND_FILE {
            return Err(Error::Refused(format!("{shown}: replaced while the volume was read")));
        }
        let mut r = Self::head(path, st, Some(&fd))?;
        if st.st_nlink > 1 {
            let ino = wide(st.st_ino);
            if let Some(first) = self.linked.get(&ino) {
                r.bytes(first);
                self.record(path, &r);
                return Ok(());
            }
            self.linked.insert(ino, path.to_vec());
        }
        let size = wide(st.st_size);
        r.number(size);
        r.0.extend_from_slice(&i64::from(st.st_mtime).to_be_bytes());
        r.number(wide(st.st_mtime_nsec));
        let file = File::from(fd);
        let runs = crate::file::extents(&file, size).map_err(sys(&shown))?;
        r.bytes(&data_digest(&file, &runs).map_err(|e| Error::Io { path: shown, source: e })?);
        r.number(wide(runs.len()));
        for (at, len) in runs {
            r.number(at);
            r.number(len);
        }
        self.record(path, &r);
        Ok(())
    }
}

/// The SHA-256 of the data `file` holds in `runs`, read in order.
fn data_digest(file: &File, runs: &[(u64, u64)]) -> std::io::Result<[u8; 32]> {
    use std::os::unix::fs::FileExt as _;
    let mut sha = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    for &(start, len) in runs {
        let mut done = 0;
        while done < len {
            let want = usize::try_from((len - done).min(wide(buf.len()))).unwrap_or(buf.len());
            let slice = buf.get_mut(..want).unwrap_or_default();
            let n = file.read_at(slice, start + done)?;
            if n == 0 {
                return Err(std::io::Error::other("shorter than it said: changed while read"));
            }
            sha.update(slice.get(..n).unwrap_or_default());
            done += wide(n);
        }
    }
    Ok(sha.finalize().into())
}

/// The manifest of the tree the directory `root` is open on.
///
/// # Errors
/// What the walk could not read, where.
pub fn of(root: &OwnedFd) -> Result<Manifest> {
    let dev = wide(rustix::fs::fstat(root).map_err(sys("."))?.st_dev);
    let mut walk = Walk {
        dev,
        all: Sha256::new(),
        run: Sha256::new(),
        in_run: 0,
        first: String::new(),
        chunks: Vec::new(),
        linked: HashMap::new(),
    };
    walk.dir(root, b"", 0)?;
    walk.close_run();
    Ok(Manifest { digest: hex(&walk.all.finalize()), chunks: walk.chunks })
}

#[cfg(test)]
mod tests;
