// SPDX-License-Identifier: AGPL-3.0-or-later

//! Verifying a restore (§5.4 of docs/06-storage.md, with the contract of §8 of
//! docs/05-scheduling.md): what was restored is walked as a backup would walk it, sending
//! nothing, and compared with the backup entry by entry: kind, mode, owner, modification time,
//! size, content by digest, link target, device number, extended attributes and with them ACLs,
//! and which names are links of one file. A restore that does not compare equal is not a restore.
//! Nothing is fetched but the backup's trees.

use crate::error::{Error, Result, sys};
use crate::file::Content;
use crate::name::display;
use crate::repo::Repo;
use crate::sink::Dry;
use crate::store::ObjectStore;
use crate::walk::Walker;
use korpis_proto::korpis::backup::v1 as pb;
use rustix::fs::{Mode, OFlags};
use std::collections::HashMap;
use std::path::PathBuf;

/// Checks that `restored` holds what backup `backup` holds at `path` (the whole volume when
/// empty), as a restore of it there would have created it.
pub async fn verify<S: ObjectStore + 'static>(
    repo: &Repo<S>,
    backup: &str,
    path: &str,
    restored: PathBuf,
) -> Result<()> {
    let record = repo.backup(backup).await?;
    let keys = std::sync::Arc::clone(repo.keys());
    let (root, trees) = tokio::task::spawn_blocking(move || {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let fd = rustix::fs::open(&restored, flags, Mode::empty()).map_err(sys("."))?;
        let content = Content::default();
        let mut w = Walker::new(&keys, Dry::default(), &content, &fd)?;
        let root = w.root(&fd, None)?;
        Ok::<_, Error>((root, w.sink.trees))
    })
    .await
    .map_err(|e| Error::Refused(format!("the verification stopped: {e}")))??;
    let mut c = Compare { repo, trees, links: Vec::new() };
    if path.split('/').all(str::is_empty) {
        if let Some(e) = record.root_entry.as_option() {
            same(e, &root.0, ".")?;
        }
        c.tree(&record.root, &root.1, "").await?;
        return links(&c.links);
    }
    let name = path.rsplit('/').find(|p| !p.is_empty()).unwrap_or_default();
    let ours = pb::Tree {
        entries: vec![crate::restore::find(repo, &record.root, path).await?],
        ..Default::default()
    };
    let theirs = c.trees.get(&root.1).cloned().unwrap_or_default();
    let found = theirs.entries.iter().find(|e| e.name == name.as_bytes());
    let found = found.ok_or_else(|| Error::Mismatch(format!("{path} was not restored")))?;
    c.entries(&ours, &pb::Tree { entries: vec![found.clone()], ..Default::default() }, "").await?;
    links(&c.links)
}

/// That the restored names are links of one file exactly where the backup's were, among the
/// names restored: a group two or more of whose names were restored is one restored file, and a
/// name restored alone is a file of its own.
fn links(files: &[(u64, u64, String)]) -> Result<()> {
    let mut restored: HashMap<u64, usize> = HashMap::new();
    for (b, ..) in files.iter().filter(|f| f.0 != 0) {
        *restored.entry(*b).or_default() += 1;
    }
    let mut became: HashMap<u64, u64> = HashMap::new();
    let mut from: HashMap<u64, u64> = HashMap::new();
    for (b, r, path) in files {
        let together = *b != 0 && restored.get(b).copied().unwrap_or(0) > 1;
        let fits = if together {
            *r != 0 && *became.entry(*b).or_insert(*r) == *r && *from.entry(*r).or_insert(*b) == *b
        } else {
            *r == 0
        };
        if !fits {
            return Err(differ(path, "which names are links of one file"));
        }
    }
    Ok(())
}

struct Compare<'a, S> {
    repo: &'a Repo<S>,
    trees: HashMap<String, pb::Tree>,
    /// Every file compared: its link group in the backup and as restored, and its path.
    links: Vec<(u64, u64, String)>,
}

fn differ(path: &str, what: &str) -> Error {
    Error::Mismatch(format!("{path}: {what} differs"))
}

/// That `restored` is `backed` by everything a restore restores.
fn same(backed: &pb::Entry, restored: &pb::Entry, path: &str) -> Result<()> {
    let checks: [(&str, bool); 9] = [
        ("kind", backed.kind == restored.kind),
        ("mode", backed.mode == restored.mode),
        ("owner", (backed.uid, backed.gid) == (restored.uid, restored.gid)),
        (
            "modification time",
            (backed.mtime_seconds, backed.mtime_nanos)
                == (restored.mtime_seconds, restored.mtime_nanos),
        ),
        ("size", backed.size == restored.size),
        ("content", backed.digest == restored.digest),
        ("link target", backed.link_target == restored.link_target),
        ("device", backed.rdev == restored.rdev),
        ("extended attributes", backed.xattrs == restored.xattrs),
    ];
    match checks.iter().find(|(_, ok)| !ok) {
        Some((what, _)) => Err(differ(path, what)),
        None => Ok(()),
    }
}

impl<S: ObjectStore> Compare<'_, S> {
    async fn tree(&mut self, backed: &str, restored: &str, at: &str) -> Result<()> {
        let ours = self.repo.tree(backed).await?;
        let theirs = self
            .trees
            .get(restored)
            .cloned()
            .ok_or_else(|| Error::Mismatch(format!("{at}/ was not walked")))?;
        self.entries(&ours, &theirs, at).await
    }

    async fn entries(&mut self, ours: &pb::Tree, theirs: &pb::Tree, at: &str) -> Result<()> {
        let restorable =
            |e: &&pb::Entry| e.kind.as_known() != Some(pb::EntryKind::ENTRY_KIND_SOCKET);
        let ours: Vec<&pb::Entry> = ours.entries.iter().filter(restorable).collect();
        if ours.len() != theirs.entries.len() {
            return Err(differ(&format!("{at}/"), "the set of names"));
        }
        for (b, r) in ours.into_iter().zip(&theirs.entries) {
            let path = format!("{at}/{}", display(&b.name));
            if b.name != r.name {
                return Err(differ(&format!("{at}/"), "the set of names"));
            }
            same(b, r, &path)?;
            if b.kind.as_known() == Some(pb::EntryKind::ENTRY_KIND_FILE) {
                self.links.push((b.link_group, r.link_group, path.clone()));
            }
            if !b.subtree.is_empty() {
                Box::pin(self.tree(&b.subtree, &r.subtree, &path)).await?;
            }
        }
        Ok(())
    }
}
