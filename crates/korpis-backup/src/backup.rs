// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backing up a volume (§5.1 of docs/06-storage.md): its tree read from a snapshot where its class
//! can take one, chunked, deduplicated against what the workload's repository already holds,
//! sealed, and written several objects at once; the backup's record last, so a backup whose
//! record exists is whole and one that failed leaves only objects a later backup reuses.

use crate::error::{Error, Result, sys};
use crate::file::Content;
use crate::repo::Repo;
use crate::seal::Kind;
use crate::sink::{Upload, upload};
use crate::store::ObjectStore;
use crate::walk::Walker;
use korpis_proto::korpis::backup::v1 as pb;
use rustix::fs::{Mode, OFlags};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// How many objects the walk may have queued ahead of the uploaders.
const QUEUE: usize = 32;

/// What a backup reads, and how.
#[derive(Debug, Clone)]
pub struct Options {
    /// The directory to back up: a snapshot of the volume, or the volume itself.
    pub source: PathBuf,
    /// Whether `source` is a snapshot, a single point in time.
    pub consistent: bool,
    /// The workload's last backup, whose unchanged files are not read again.
    pub parent: Option<String>,
    /// Recipe content files may be recorded by reference to.
    pub content: Content,
    /// How many objects are sealed and written at once.
    pub concurrency: usize,
    /// The filesystem a block device's image holds, recorded as given; empty for files.
    pub image_filesystem: String,
}

fn timestamp(t: SystemTime) -> buffa_types::google::protobuf::Timestamp {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    buffa_types::google::protobuf::Timestamp {
        seconds: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(d.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    }
}

/// Backs up `options.source` into `repo` as backup `id`.
pub async fn backup<S: ObjectStore + 'static>(
    repo: &Repo<S>,
    id: &str,
    options: Options,
) -> Result<pb::Backup> {
    let started = SystemTime::now();
    let mut known: std::collections::HashSet<String> =
        repo.ids(Kind::Chunk).await?.iter().map(|c| Kind::Chunk.key_of(c)).collect();
    known.extend(repo.ids(Kind::Tree).await?.iter().map(|t| Kind::Tree.key_of(t)));
    let parent = match &options.parent {
        Some(p) => Some(repo.backup(p).await?.root),
        None => None,
    };
    let (tx, rx) = mpsc::channel(QUEUE);
    let uploaders = tokio::spawn(upload(repo.clone(), rx, options.concurrency));
    let sink = Upload {
        known,
        tx,
        repo: repo.clone(),
        handle: tokio::runtime::Handle::current(),
        new_chunks: 0,
        new_bytes: 0,
    };
    let keys = std::sync::Arc::clone(repo.keys());
    let walk = tokio::task::spawn_blocking(move || {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let root = rustix::fs::open(&options.source, flags, Mode::empty()).map_err(sys("."))?;
        let mut w = Walker::new(&keys, sink, &options.content, &root)?;
        let (entry, tree) = w.root(&root, parent.as_deref())?;
        let mut stats = w.stats;
        (stats.chunks_new, stats.bytes_new) = (w.sink.new_chunks, w.sink.new_bytes);
        Ok::<_, Error>((entry, tree, stats, options.consistent))
    });
    let walked = walk.await.map_err(|e| Error::Refused(format!("the walk stopped: {e}")));
    let written =
        uploaders.await.map_err(|e| Error::Refused(format!("the upload stopped: {e}")))?;
    // The uploaders' failure is the cause when both failed: the walk stops because they did.
    let stored = written?;
    let (entry, root, mut stats, consistent) = walked??;
    stats.stored_bytes = stored;
    let record = pb::Backup {
        id: id.to_owned(),
        root,
        started: timestamp(started).into(),
        finished: timestamp(SystemTime::now()).into(),
        consistent,
        stats: stats.into(),
        root_entry: entry.into(),
        image_filesystem: options.image_filesystem.clone(),
        ..Default::default()
    };
    repo.put_backup(&record).await?;
    Ok(record)
}
