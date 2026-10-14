// SPDX-License-Identifier: AGPL-3.0-or-later

//! Restoring a backup (§5.4 of docs/06-storage.md): the whole volume into an empty directory, or
//! one path of it beside what is there, never over it. Every name is created relative to a
//! directory this restore created, exclusively, so a tree cannot direct a write anywhere else,
//! and the trees are checked before any name in them is created.

use crate::apply::{ContentSource, Source, data, inode, node};
use crate::error::{Error, Result, sys};
use crate::name::{check_name, display};
use crate::repo::Repo;
use crate::store::ObjectStore;
use crate::walk::os;
use korpis_proto::korpis::backup::v1 as pb;
use rustix::fs::{FileType, Mode, OFlags};
use std::collections::HashMap;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::Arc;

const DEPTH: usize = 2048;

/// What to restore, and where.
#[derive(Clone)]
pub struct Restore {
    /// The backup.
    pub backup: String,
    /// One path in it, `/`-separated from the volume's root; the whole volume when empty.
    pub path: String,
    /// The directory restored into. For the whole volume it must be empty; a path is restored
    /// into it under its own name, which must not already be there.
    pub target: PathBuf,
    /// Where recipe content the backup recorded by reference is read from.
    pub content: Arc<dyn ContentSource>,
    /// How many chunks are fetched at once.
    pub concurrency: usize,
}

impl std::fmt::Debug for Restore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Restore")
            .field("backup", &self.backup)
            .field("path", &self.path)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

/// What a restore created.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Restored {
    /// Files, directories, and their bytes.
    pub files: u64,
    pub directories: u64,
    pub bytes: u64,
    /// What the backup holds that a restore does not create, and why.
    pub skipped: Vec<String>,
}

struct Restorer<S> {
    repo: Repo<S>,
    handle: tokio::runtime::Handle,
    content: Arc<dyn ContentSource>,
    concurrency: usize,
    /// The first name restored of each link group, relative to the target.
    links: HashMap<u64, Vec<u8>>,
    root: OwnedFd,
    done: Restored,
}

fn open_dir(dir: &OwnedFd, name: &[u8]) -> rustix::io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::openat(dir, os(name), flags, Mode::empty())
}

/// The entry at `path` in the tree `root`, found tree by tree.
pub(crate) async fn find<S: ObjectStore>(
    repo: &Repo<S>,
    root: &str,
    path: &str,
) -> Result<pb::Entry> {
    let mut tree = repo.tree(root).await?;
    // Collected before the first await, so no iterator is held across one.
    let parts: Vec<String> = path.split('/').filter(|p| !p.is_empty()).map(str::to_owned).collect();
    for (i, part) in parts.iter().enumerate() {
        check_name(part.as_bytes())
            .map_err(|_| Error::Refused(format!("{path:?} is not a path in a volume")))?;
        let entry = tree.entries.iter().find(|e| e.name == part.as_bytes()).cloned();
        let entry = entry.ok_or_else(|| Error::Refused(format!("{path} is not in the backup")))?;
        if i + 1 == parts.len() {
            return Ok(entry);
        }
        if entry.subtree.is_empty() {
            return Err(Error::Refused(format!("{path} is not in the backup")));
        }
        tree = repo.tree(&entry.subtree).await?;
    }
    Err(Error::Refused("an empty path names the whole volume".into()))
}

/// Restores `what` from `repo`.
pub async fn restore<S: ObjectStore + 'static>(repo: &Repo<S>, what: Restore) -> Result<Restored> {
    let backup = repo.backup(&what.backup).await?;
    let whole = what.path.split('/').all(str::is_empty);
    let entry = if whole { None } else { Some(find(repo, &backup.root, &what.path).await?) };
    let repo = repo.clone();
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let root = rustix::fs::open(&what.target, flags, Mode::empty()).map_err(sys("."))?;
        let mut r = Restorer {
            repo,
            handle,
            content: what.content,
            concurrency: what.concurrency,
            links: HashMap::new(),
            root,
            done: Restored::default(),
        };
        let root = r.root.try_clone().map_err(|e| Error::Io { path: ".".into(), source: e })?;
        if let Some(e) = entry {
            r.entry(&root, b"", &e, 0)?;
            return Ok(r.done);
        }
        if !empty(&root)? {
            return Err(Error::Refused(format!("{} is not empty", what.target.display())));
        }
        r.tree(&root, b"", &backup.root, 0)?;
        if let Some(e) = backup.root_entry.as_option() {
            inode(&root, e, ".")?;
        }
        Ok(r.done)
    })
    .await
    .map_err(|e| Error::Refused(format!("the restore stopped: {e}")))?
}

/// Whether the directory `dir` is open on holds nothing.
fn empty(dir: &OwnedFd) -> Result<bool> {
    for entry in rustix::fs::Dir::read_from(dir).map_err(sys("."))? {
        if !matches!(entry.map_err(sys("."))?.file_name().to_bytes(), b"." | b"..") {
            return Ok(false);
        }
    }
    Ok(true)
}

fn join(at: &[u8], name: &[u8]) -> Vec<u8> {
    if at.is_empty() { name.to_vec() } else { [at, b"/", name].concat() }
}

impl<S: ObjectStore> Restorer<S> {
    /// Restores tree `id` into the directory `dir` is open on, at `at` within the target.
    fn tree(&mut self, dir: &OwnedFd, at: &[u8], id: &str, depth: usize) -> Result<()> {
        if depth > DEPTH {
            return Err(Error::Corrupt(format!("deeper than {DEPTH} directories")));
        }
        let tree = self.handle.block_on(self.repo.tree(id))?;
        for e in &tree.entries {
            self.entry(dir, at, e, depth)?;
        }
        Ok(())
    }

    /// Restores `e` into the directory `dir` is open on, at `at` within the target.
    fn entry(&mut self, dir: &OwnedFd, at: &[u8], e: &pb::Entry, depth: usize) -> Result<()> {
        check_name(&e.name)?;
        let rel = join(at, &e.name);
        let path = display(&rel);
        let name = e.name.as_slice();
        match e.kind.as_known() {
            Some(pb::EntryKind::ENTRY_KIND_DIRECTORY) => {
                rustix::fs::mkdirat(dir, os(name), Mode::from_raw_mode(0o700))
                    .map_err(sys(&path))?;
                let fd = open_dir(dir, name).map_err(sys(&path))?;
                if !e.subtree.is_empty() {
                    self.tree(&fd, &rel, &e.subtree, depth + 1)?;
                }
                inode(&fd, e, &path)?;
                self.done.directories += 1;
            }
            Some(pb::EntryKind::ENTRY_KIND_FILE) => self.file(dir, rel, e, &path)?,
            Some(pb::EntryKind::ENTRY_KIND_SYMLINK) => {
                rustix::fs::symlinkat(os(&e.link_target), dir, os(name)).map_err(sys(&path))?;
                node(dir, name, e, &path)?;
            }
            Some(
                k @ (pb::EntryKind::ENTRY_KIND_FIFO
                | pb::EntryKind::ENTRY_KIND_CHAR_DEVICE
                | pb::EntryKind::ENTRY_KIND_BLOCK_DEVICE),
            ) => {
                let kind = match k {
                    pb::EntryKind::ENTRY_KIND_FIFO => FileType::Fifo,
                    pb::EntryKind::ENTRY_KIND_CHAR_DEVICE => FileType::CharacterDevice,
                    _ => FileType::BlockDevice,
                };
                rustix::fs::mknodat(dir, os(name), kind, Mode::from_raw_mode(0o600), e.rdev)
                    .map_err(sys(&path))?;
                node(dir, name, e, &path)?;
            }
            _ => self
                .done
                .skipped
                .push(format!("{path}: a socket, or of an unknown kind, is not restored")),
        }
        Ok(())
    }

    fn file(&mut self, dir: &OwnedFd, rel: Vec<u8>, e: &pb::Entry, path: &str) -> Result<()> {
        let name = e.name.as_slice();
        if e.link_group != 0
            && let Some(first) = self.links.get(&e.link_group)
        {
            rustix::fs::linkat(&self.root, os(first), dir, os(name), rustix::fs::AtFlags::empty())
                .map_err(sys(path))?;
            return Ok(());
        }
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(dir, os(name), flags, Mode::from_raw_mode(0o600))
            .map_err(sys(path))?;
        let file = File::from(fd);
        let from = Source {
            repo: &self.repo,
            handle: &self.handle,
            concurrency: self.concurrency,
            content: self.content.as_ref(),
        };
        data(&from, &file, e, path)?;
        inode(&file, e, path)?;
        if e.link_group != 0 {
            self.links.insert(e.link_group, rel);
        }
        self.done.files += 1;
        self.done.bytes += e.size;
        Ok(())
    }
}
