// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a backup without restoring it (§5.4 of docs/06-storage.md): one directory's entries,
//! which is browsing, and one file's bytes, which is reading. Each fetches the trees on the way
//! and nothing else; a file's chunks are fetched in order and sent as they arrive, its holes as
//! zeros, and the whole is checked against the digest the backup recorded, so a reader that got
//! every piece and no error got the file.

use crate::error::{Error, Result};
use crate::repo::Repo;
use crate::restore::find;
use crate::store::ObjectStore;
use korpis_proto::korpis::backup::v1 as pb;
use tokio::sync::mpsc;

/// How many zero bytes go in one piece of a hole.
const ZEROS: usize = 1 << 20;

fn is_root(path: &str) -> bool {
    path.split('/').all(str::is_empty)
}

/// The entries of the directory at `path` in backup `backup`; the volume's root when empty.
///
/// # Errors
/// The path is not a directory in the backup, or the repository could not be read.
pub async fn list<S: ObjectStore>(
    repo: &Repo<S>,
    backup: &str,
    path: &str,
) -> Result<Vec<pb::Entry>> {
    let b = repo.backup(backup).await?;
    let tree = if is_root(path) {
        b.root
    } else {
        let e = find(repo, &b.root, path).await?;
        if e.kind.as_known() != Some(pb::EntryKind::ENTRY_KIND_DIRECTORY) {
            return Err(Error::Refused(format!("{path} is not a directory")));
        }
        e.subtree
    };
    Ok(repo.tree(&tree).await?.entries)
}

/// The file at `path` in backup `backup`.
///
/// # Errors
/// The path is not a file in the backup, or the repository could not be read.
pub async fn file<S: ObjectStore>(repo: &Repo<S>, backup: &str, path: &str) -> Result<pb::Entry> {
    let b = repo.backup(backup).await?;
    let e = find(repo, &b.root, path).await?;
    if e.kind.as_known() != Some(pb::EntryKind::ENTRY_KIND_FILE) {
        return Err(Error::Refused(format!("{path} is not a file")));
    }
    if !e.content.is_empty() {
        return Err(Error::Refused(format!("{path} is recipe content, kept by reference")));
    }
    Ok(e)
}

/// Where the pieces of a file go, and the digest they make.
struct Out<'a> {
    to: &'a mpsc::Sender<Vec<u8>>,
    digest: blake3::Hasher,
    at: u64,
}

impl Out<'_> {
    async fn send(&mut self, piece: Vec<u8>) -> Result<()> {
        self.digest.update(&piece);
        self.at += u64::try_from(piece.len()).unwrap_or(u64::MAX);
        self.to.send(piece).await.map_err(|_| Error::Refused("the reader went away".into()))
    }

    /// Zeros up to offset `to`.
    async fn zeros_to(&mut self, to: u64) -> Result<()> {
        while self.at < to {
            let n = usize::try_from(to - self.at).unwrap_or(ZEROS).min(ZEROS);
            self.send(vec![0; n]).await?;
        }
        Ok(())
    }
}

/// Sends the bytes of file `e` to `to` in order, holes as zeros; its length.
///
/// # Errors
/// A chunk could not be read, the reader went away, or what was read is not what the backup
/// recorded.
pub async fn send<S: ObjectStore>(
    repo: &Repo<S>,
    e: &pb::Entry,
    to: &mpsc::Sender<Vec<u8>>,
) -> Result<u64> {
    let mut out = Out { to, digest: repo.keys().digester(), at: 0 };
    // No runs recorded is data throughout, or a file all hole, which has no chunks.
    let runs: Vec<(u64, u64)> = if e.extents.is_empty() && e.chunks.is_empty() {
        Vec::new()
    } else if e.extents.is_empty() {
        vec![(0, e.size)]
    } else {
        e.extents.iter().map(|x| (x.offset, x.length)).collect()
    };
    let mut chunks = e.chunks.iter();
    for (offset, length) in runs {
        out.zeros_to(offset).await?;
        while out.at < offset + length {
            let id =
                chunks.next().ok_or_else(|| Error::Refused("a file short of its chunks".into()))?;
            out.send(repo.chunk(id).await?).await?;
        }
    }
    out.zeros_to(e.size).await?;
    if out.at != e.size || out.digest.finalize().as_bytes().as_slice() != e.digest.as_slice() {
        return Err(Error::Refused("what was read is not what the backup recorded".into()));
    }
    Ok(out.at)
}
