// SPDX-License-Identifier: AGPL-3.0-or-later

//! The walk of a volume: every directory, depth first, each read through a descriptor opened
//! relative to its parent's with `O_NOFOLLOW`, so the walk stays inside the tree it began at
//! whatever the workload renames or links while it runs, and never leaves its filesystem. Each
//! directory becomes a tree, written after its entries, so a tree is never written before what it
//! names. A file the parent backup holds with the same size, times, and inode is not read again.

use crate::error::{Error, Result, sys};
use crate::file::{Content, read};
use crate::keys::Keys;
use crate::meta::{entry_of, kind_of, wide, xattrs};
use crate::name::display;
use crate::seal::Kind;
use buffa::Message as _;
use korpis_proto::korpis::backup::v1 as pb;
use rustix::fs::{AtFlags, Mode, OFlags, Stat};
use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsFd as _, OwnedFd};

/// The deepest a walk goes; a path is at most 4096 bytes, so a tree deeper than this is not one a
/// volume can hold.
const DEPTH: usize = 2048;

/// Where a walk sends what it finds.
pub(crate) trait Sink {
    /// Whether the repository has, or is being sent, object `kind`/`id`.
    fn has(&mut self, kind: Kind, id: &str) -> bool;
    /// Sends object `kind`/`id`.
    fn put(&mut self, kind: Kind, id: &str, plain: Vec<u8>) -> Result<()>;
    /// The parent backup's tree `id`, if the walk compares against one.
    fn parent(&mut self, id: &str) -> Result<Option<pb::Tree>>;
}

/// One walk.
pub(crate) struct Walker<'a, K> {
    pub keys: &'a Keys,
    pub sink: K,
    pub content: &'a Content,
    pub stats: pb::BackupStats,
    dev: u64,
    /// The first entry of each hard-linked inode, by device and inode.
    groups: HashMap<(u64, u64), pb::Entry>,
}

fn open_dir(at: &OwnedFd, name: &[u8]) -> rustix::io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::openat(at, os(name), flags, Mode::empty())
}

/// A name as the system takes it.
pub(crate) fn os(name: &[u8]) -> &std::ffi::OsStr {
    std::os::unix::ffi::OsStrExt::from_bytes(name)
}

/// The names in the directory `fd` is open on, in order.
fn names(fd: &OwnedFd) -> rustix::io::Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for entry in rustix::fs::Dir::read_from(fd)? {
        let name = entry?.file_name().to_bytes().to_vec();
        if name != b"." && name != b".." {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

/// The parent's entry named `name`, if it has one.
fn previous<'t>(parent: Option<&'t pb::Tree>, name: &[u8]) -> Option<&'t pb::Entry> {
    let entries = &parent?.entries;
    entries.binary_search_by(|e| e.name.as_slice().cmp(name)).ok().and_then(|i| entries.get(i))
}

impl<'a, K: Sink> Walker<'a, K> {
    /// A walk of the directory `root` is open on, never leaving its filesystem.
    pub fn new(keys: &'a Keys, sink: K, content: &'a Content, root: &OwnedFd) -> Result<Self> {
        let dev = wide(rustix::fs::fstat(root).map_err(sys("."))?.st_dev);
        Ok(Self {
            keys,
            sink,
            content,
            stats: pb::BackupStats::default(),
            dev,
            groups: HashMap::new(),
        })
    }

    /// The root directory's entry and tree, compared against the parent backup's root `parent`.
    pub fn root(&mut self, root: &OwnedFd, parent: Option<&str>) -> Result<(pb::Entry, String)> {
        let st = rustix::fs::fstat(root).map_err(sys("."))?;
        let mut entry = entry_of(b".", &st);
        entry.xattrs = xattrs(root.as_fd()).map_err(sys("."))?;
        let parent = match parent {
            Some(id) => self.sink.parent(id)?,
            None => None,
        };
        let tree = self.dir(root, "", parent.as_ref(), 0)?;
        entry.subtree.clone_from(&tree);
        Ok((entry, tree))
    }

    fn skip(&mut self, path: &str, why: &str) {
        tracing::debug!(%path, %why, "not backed up");
        self.stats.skipped.push(format!("{path}: {why}"));
    }

    /// The tree of the directory `fd` is open on, at `path`.
    fn dir(
        &mut self,
        fd: &OwnedFd,
        path: &str,
        parent: Option<&pb::Tree>,
        depth: usize,
    ) -> Result<String> {
        if depth > DEPTH {
            return Err(Error::Refused(format!("{path}: deeper than {DEPTH} directories")));
        }
        let mut tree = pb::Tree::default();
        for name in names(fd).map_err(sys(path))? {
            let at = format!("{path}/{}", display(&name));
            if let Some(entry) = self.entry(fd, &name, &at, previous(parent, &name), depth)? {
                tree.entries.push(entry);
            }
        }
        self.stats.directories += 1;
        let encoded = tree.encode_to_vec();
        let id = self.keys.tree_id(&encoded);
        if !self.sink.has(Kind::Tree, &id) {
            self.sink.put(Kind::Tree, &id, encoded)?;
        }
        Ok(id)
    }

    /// The entry for `name` in the directory `dir` is open on, or `None` when it is not backed up.
    fn entry(
        &mut self,
        dir: &OwnedFd,
        name: &[u8],
        path: &str,
        prev: Option<&pb::Entry>,
        depth: usize,
    ) -> Result<Option<pb::Entry>> {
        let st = match rustix::fs::statat(dir, os(name), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => st,
            Err(rustix::io::Errno::NOENT) => {
                self.skip(path, "removed while the volume was read");
                return Ok(None);
            }
            Err(e) => return Err(sys(path)(e)),
        };
        if wide(st.st_dev) != self.dev {
            self.skip(path, "on another filesystem");
            return Ok(None);
        }
        match kind_of(&st) {
            pb::EntryKind::ENTRY_KIND_DIRECTORY => self.subdir(dir, name, path, prev, depth),
            pb::EntryKind::ENTRY_KIND_FILE => self.file(dir, name, path, prev),
            pb::EntryKind::ENTRY_KIND_SYMLINK => {
                let target = rustix::fs::readlinkat(dir, os(name), Vec::new());
                let mut e = entry_of(name, &st);
                e.link_target = target.map_err(sys(path))?.into_bytes();
                Ok(Some(e))
            }
            pb::EntryKind::ENTRY_KIND_UNSPECIFIED => {
                self.skip(path, "of an unknown kind");
                Ok(None)
            }
            _ => Ok(Some(entry_of(name, &st))),
        }
    }

    fn subdir(
        &mut self,
        dir: &OwnedFd,
        name: &[u8],
        path: &str,
        prev: Option<&pb::Entry>,
        depth: usize,
    ) -> Result<Option<pb::Entry>> {
        let fd = match open_dir(dir, name) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP) => {
                self.skip(path, "replaced while the volume was read");
                return Ok(None);
            }
            Err(e) => return Err(sys(path)(e)),
        };
        let st = rustix::fs::fstat(&fd).map_err(sys(path))?;
        if wide(st.st_dev) != self.dev {
            self.skip(path, "on another filesystem");
            return Ok(None);
        }
        let mut e = entry_of(name, &st);
        e.xattrs = xattrs(fd.as_fd()).map_err(sys(path))?;
        let parent = match prev.filter(|p| !p.subtree.is_empty()) {
            Some(p) => self.sink.parent(&p.subtree)?,
            None => None,
        };
        e.subtree = self.dir(&fd, path, parent.as_ref(), depth + 1)?;
        Ok(Some(e))
    }

    fn file(
        &mut self,
        dir: &OwnedFd,
        name: &[u8],
        path: &str,
        prev: Option<&pb::Entry>,
    ) -> Result<Option<pb::Entry>> {
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        let fd = match rustix::fs::openat(dir, os(name), flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT | rustix::io::Errno::LOOP) => {
                self.skip(path, "replaced while the volume was read");
                return Ok(None);
            }
            Err(e) => return Err(sys(path)(e)),
        };
        let st = rustix::fs::fstat(&fd).map_err(sys(path))?;
        if kind_of(&st) != pb::EntryKind::ENTRY_KIND_FILE || wide(st.st_dev) != self.dev {
            self.skip(path, "replaced while the volume was read");
            return Ok(None);
        }
        let mut e = entry_of(name, &st);
        e.xattrs = xattrs(fd.as_fd()).map_err(sys(path))?;
        self.data(&File::from(fd), &st, &mut e, prev, path)?;
        self.stats.files += 1;
        self.stats.bytes += e.size;
        Ok(Some(e))
    }

    /// Fills a file's data: another name's for a hard link already met, the parent's for a file
    /// unchanged since, and otherwise what reading it finds.
    fn data(
        &mut self,
        file: &File,
        st: &Stat,
        e: &mut pb::Entry,
        prev: Option<&pb::Entry>,
        path: &str,
    ) -> Result<()> {
        let key = (wide(st.st_dev), wide(st.st_ino));
        let links = wide(st.st_nlink);
        if links > 1 {
            e.links = u32::try_from(links).unwrap_or(u32::MAX);
            if let Some(first) = self.groups.get(&key) {
                copy_data(e, first);
                return Ok(());
            }
            e.link_group = wide(self.groups.len()) + 1;
        }
        if let Some(p) = prev.filter(|p| unchanged(p, e)) {
            copy_data(e, p);
            self.stats.files_unchanged += 1;
        } else {
            let data = read(self.keys, &mut self.sink, file, e.size, self.content).map_err(
                |err| match err {
                    Error::Io { source, .. } => Error::Io { path: path.to_owned(), source },
                    other => other,
                },
            )?;
            if data.short {
                self.skip(path, "changed while it was read; restored as it was read");
            }
            (e.chunks, e.extents, e.digest, e.content) =
                (data.chunks, data.extents, data.digest, data.content);
        }
        if links > 1 {
            self.groups.insert(key, e.clone());
        }
        Ok(())
    }
}

/// Whether `now` is the file `before` was, by everything that changes when a file does.
fn unchanged(before: &pb::Entry, now: &pb::Entry) -> bool {
    before.kind == now.kind
        && before.size == now.size
        && (before.mtime_seconds, before.mtime_nanos) == (now.mtime_seconds, now.mtime_nanos)
        && (before.ctime_seconds, before.ctime_nanos) == (now.ctime_seconds, now.ctime_nanos)
        && before.ino == now.ino
        && !before.digest.is_empty()
}

fn copy_data(e: &mut pb::Entry, from: &pb::Entry) {
    e.chunks.clone_from(&from.chunks);
    e.extents.clone_from(&from.extents);
    e.digest.clone_from(&from.digest);
    e.content.clone_from(&from.content);
    if e.link_group == 0 {
        e.link_group = from.link_group;
    }
}
