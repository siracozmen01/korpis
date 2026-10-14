// SPDX-License-Identifier: AGPL-3.0-or-later

//! Making a restored inode what the backup recorded: its data, then its owner, then its mode,
//! then its attributes, then its times, in that order because each undoes part of the one before
//! it otherwise: changing the owner clears set-id bits and file capabilities, setting an ACL sets
//! the group bits of the mode, and every write moves the times.

use crate::error::{Error, Result, sys};
use crate::repo::Repo;
use crate::store::ObjectStore;
use crate::walk::os;
use korpis_proto::korpis::backup::v1 as pb;
use rustix::fs::{AtFlags, Gid, Mode, Timespec, Timestamps, Uid, XattrFlags};
use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::FileExt as _;

/// Where recipe content a backup recorded by reference is read from at restore (§5.2 of
/// docs/06-storage.md).
pub trait ContentSource: Send + Sync {
    /// The file holding the content whose sha256 is `sha256`, if there is one.
    fn open(&self, sha256: &str) -> std::io::Result<Option<File>>;
}

/// No content: a backup that recorded any by reference cannot be restored from this.
#[derive(Debug, Clone, Copy)]
pub struct NoContent;

impl ContentSource for NoContent {
    fn open(&self, _: &str) -> std::io::Result<Option<File>> {
        Ok(None)
    }
}

fn times(e: &pb::Entry) -> Timestamps {
    let at = Timespec { tv_sec: e.mtime_seconds, tv_nsec: e.mtime_nanos.into() };
    Timestamps { last_access: at, last_modification: at }
}

fn owner(e: &pb::Entry) -> (Option<Uid>, Option<Gid>) {
    (Some(Uid::from_raw_unchecked(e.uid)), Some(Gid::from_raw_unchecked(e.gid)))
}

/// Owner, mode, attributes, and times of the file or directory `fd` is open on.
pub(crate) fn inode(fd: impl AsFd, e: &pb::Entry, path: &str) -> Result<()> {
    let (uid, gid) = owner(e);
    rustix::fs::fchown(&fd, uid, gid).map_err(sys(path))?;
    rustix::fs::fchmod(&fd, Mode::from_raw_mode(e.mode)).map_err(sys(path))?;
    for x in &e.xattrs {
        rustix::fs::fsetxattr(&fd, os(&x.name), &x.value, XattrFlags::empty())
            .map_err(sys(path))?;
    }
    rustix::fs::futimens(&fd, &times(e)).map_err(sys(path))
}

/// Owner, mode where it has one, and times of `name` in `dir`, which is not opened: a link, a
/// device, a pipe.
pub(crate) fn node(dir: &OwnedFd, name: &[u8], e: &pb::Entry, path: &str) -> Result<()> {
    let (uid, gid) = owner(e);
    rustix::fs::chownat(dir, os(name), uid, gid, AtFlags::SYMLINK_NOFOLLOW).map_err(sys(path))?;
    if e.kind.as_known() != Some(pb::EntryKind::ENTRY_KIND_SYMLINK) {
        rustix::fs::chmodat(dir, os(name), Mode::from_raw_mode(e.mode), AtFlags::empty())
            .map_err(sys(path))?;
    }
    rustix::fs::utimensat(dir, os(name), &times(e), AtFlags::SYMLINK_NOFOLLOW).map_err(sys(path))
}

/// Where each chunk of a file goes: the runs its data was read from, or one run from the start.
fn runs(e: &pb::Entry) -> Vec<(u64, u64)> {
    if e.extents.is_empty() {
        vec![(0, u64::MAX)]
    } else {
        e.extents.iter().map(|x| (x.offset, x.length)).collect()
    }
}

/// Where a restore reads data from.
pub(crate) struct Source<'a, S> {
    pub repo: &'a Repo<S>,
    pub handle: &'a tokio::runtime::Handle,
    /// How many chunks are fetched at once.
    pub concurrency: usize,
    pub content: &'a dyn ContentSource,
}

/// Writes a file's data, fetching several chunks at once, and gives it its length; holes are
/// left as holes.
pub(crate) fn data<S: ObjectStore>(
    from: &Source<'_, S>,
    file: &File,
    e: &pb::Entry,
    path: &str,
) -> Result<()> {
    let io = |source| Error::Io { path: path.to_owned(), source };
    if !e.content.is_empty() {
        let mut content = from.content.open(&e.content).map_err(io)?.ok_or_else(|| {
            Error::Refused(format!("{path}: recipe content {} is not available", e.content))
        })?;
        let mut to = file;
        std::io::copy(&mut content, &mut to).map_err(io)?;
        return file.set_len(e.size).map_err(io);
    }
    let runs = runs(e);
    let mut place = Placer { runs: runs.iter(), at: None };
    from.handle.block_on(async {
        use futures_util::{StreamExt as _, TryStreamExt as _};
        let fetched = futures_util::stream::iter(e.chunks.iter())
            .map(|id| from.repo.chunk(id))
            .buffered(from.concurrency.max(1));
        fetched
            .try_for_each(|bytes| {
                let r = place
                    .next(crate::meta::wide(bytes.len()), path)
                    .and_then(|offset| file.write_all_at(&bytes, offset).map_err(io));
                std::future::ready(r)
            })
            .await
    })?;
    if place.unfilled() {
        return Err(Error::Corrupt(format!("{path}: its chunks do not fill its data")));
    }
    file.set_len(e.size).map_err(io)
}

/// Places chunks into a file's runs in order.
struct Placer<'a> {
    runs: std::slice::Iter<'a, (u64, u64)>,
    /// The run being filled: where the next chunk goes, and where the run ends.
    at: Option<(u64, u64)>,
}

impl Placer<'_> {
    fn next(&mut self, len: u64, path: &str) -> Result<u64> {
        let corrupt = || Error::Corrupt(format!("{path}: its chunks overrun its data"));
        let (at, end) = match self.at {
            Some(r) if r.0 < r.1 => r,
            _ => {
                // A run read as empty, a file that shrank while it was read, holds no chunk.
                let &(offset, length) = self.runs.find(|r| r.1 > 0).ok_or_else(corrupt)?;
                (offset, offset.saturating_add(length))
            }
        };
        let after = at.checked_add(len).filter(|a| *a <= end).ok_or_else(corrupt)?;
        self.at = Some((after, end));
        Ok(at)
    }

    /// Whether a run that should hold data was left short. A file read as one run has no end.
    fn unfilled(&mut self) -> bool {
        let open = self.at.is_some_and(|(at, end)| at < end && end != u64::MAX);
        open || self.runs.any(|&(_, length)| length > 0 && length != u64::MAX)
    }
}
