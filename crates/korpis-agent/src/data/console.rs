// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a console over the data path: the handler of `workload.console.read` (§2 and §8 of
//! docs/14-streams.md). The session's params are a `ConsoleRead`; the node reads the workload's
//! stream from its own segment files, the one being written included, and sends what it finds as
//! `ConsoleChunk`s, in offset order, each saying where to read from next. Offsets asked for that
//! the node no longer holds are said to be missing rather than skipped in silence. Following, it
//! reads again as soon as the stream's directory changes ([`super::wake`]), and at least every
//! second whatever it hears, until the client is done or the session ends.
//!
//! A search is a scan of the files the node holds, with the pattern bounded as `korpis-stream`
//! bounds it. Gap records are sent whatever the pattern, so a search says where it could not look.

use super::handler::{Ended, Handler, Io, Opened, Serving};
use super::wake::Wake;
use crate::reconcile::streams::Located;
use buffa::Message as _;
use buffa_types::google::protobuf::Timestamp;
use korpis_proto::korpis::v1 as pb;
use korpis_stream::dir::StreamDir;
use korpis_stream::read::{Batch, end_offset, offset_since, read};
use korpis_stream::record::{Gap, Kind, Record};
use korpis_stream::search::Matcher;
use std::path::PathBuf;
use std::time::Duration;

/// The action.
pub const ACTION: &str = "workload.console.read";

/// The longest a follower waits without looking, in case a change was not heard.
const FALLBACK: Duration = Duration::from_secs(1);
/// Records read from disk at a time.
const READ: usize = 256;
/// A chunk is sent once its bodies reach this many bytes.
const CHUNK_BYTES: usize = 512 * 1024;

/// Serves `workload.console.read` from the streams the node writes.
#[derive(Debug, Clone)]
pub struct ConsoleRead {
    located: Located,
}

impl ConsoleRead {
    /// Reads the streams `located` says where to find.
    #[must_use]
    pub const fn new(located: Located) -> Self {
        Self { located }
    }
}

impl Handler for ConsoleRead {
    fn serve(&self, opened: Opened, io: Io) -> Serving {
        let dir = self.located.borrow().get(&opened.workload).cloned();
        Box::pin(serve(dir, opened, io))
    }
}

fn ended(code: &str, message: impl Into<String>) -> Ended {
    Ended { code: code.to_owned(), message: message.into() }
}

fn nanos(t: &Timestamp) -> i64 {
    t.seconds.saturating_mul(1_000_000_000).saturating_add(i64::from(t.nanos))
}

/// The wire form of `r`.
fn record(r: Record) -> pb::StreamRecord {
    let kind = match r.kind {
        Kind::Text => pb::StreamRecordKind::STREAM_RECORD_KIND_TEXT,
        Kind::Structured => pb::StreamRecordKind::STREAM_RECORD_KIND_STRUCTURED,
        Kind::Gap => pb::StreamRecordKind::STREAM_RECORD_KIND_GAP,
        Kind::Control => pb::StreamRecordKind::STREAM_RECORD_KIND_CONTROL,
    };
    let gap = (r.kind == Kind::Gap).then(|| Gap::parse(&r.body)).flatten().map(|g| pb::StreamGap {
        records: g.records,
        bytes: g.bytes,
        millis: g.millis,
        reason: g.reason,
        ..Default::default()
    });
    pb::StreamRecord {
        offset: r.offset,
        time: Timestamp {
            seconds: r.time_nanos.div_euclid(1_000_000_000),
            nanos: i32::try_from(r.time_nanos.rem_euclid(1_000_000_000)).unwrap_or(0),
            ..Default::default()
        }
        .into(),
        source: r.source.name(),
        kind: kind.into(),
        body: r.body,
        gap: gap.into(),
        ..Default::default()
    }
}

/// Runs `f` on the blocking pool: segment files are read with ordinary file calls.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, Ended> {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(ended("unavailable", format!("the stream could not be read: {e}"))),
        Err(e) => Err(ended("unavailable", format!("the stream reader failed: {e}"))),
    }
}

/// One reader's progress through the stream.
struct Reader {
    dir: PathBuf,
    matcher: Matcher,
    next: u64,
    left: Option<u64>,
    caught_up: bool,
}

impl Reader {
    /// The next records from disk.
    async fn batch(&self) -> Result<Batch, Ended> {
        let (dir, from) = (self.dir.clone(), self.next);
        blocking(move || read(&StreamDir::new(&dir), from, READ)).await
    }

    /// What of `records` the reader keeps, within its limit, moving its position past what it
    /// examined.
    fn take(&mut self, records: Vec<Record>, next: u64) -> Vec<Record> {
        let mut kept = Vec::new();
        let mut examined = self.next;
        for r in records {
            if self.left == Some(0) {
                break;
            }
            examined = r.offset + 1;
            if self.matcher.keeps(&r) {
                self.left = self.left.map(|n| n - 1);
                kept.push(r);
            }
        }
        self.next = if self.left == Some(0) { examined } else { next };
        kept
    }

    /// Sends what `batch` holds that the reader keeps, in chunks; whether to go on.
    async fn send(&mut self, batch: Batch, io: &Io) -> bool {
        let read_all = batch.records.len() < READ;
        let missing =
            batch.missing.map(|(first, end)| pb::OffsetRange { first, end, ..Default::default() });
        let kept = self.take(batch.records, batch.next);
        let mut chunks: Vec<Vec<pb::StreamRecord>> = Vec::new();
        let (mut current, mut bytes) = (Vec::new(), 0);
        for r in kept {
            if bytes >= CHUNK_BYTES {
                chunks.push(std::mem::take(&mut current));
                bytes = 0;
            }
            bytes += r.body.len();
            current.push(record(r));
        }
        chunks.push(current);
        let last = chunks.len() - 1;
        for (i, records) in chunks.into_iter().enumerate() {
            let next_offset = if i == last {
                self.next
            } else {
                records.last().map_or(self.next, |r| r.offset + 1)
            };
            let mut chunk = pb::ConsoleChunk { records, next_offset, ..Default::default() };
            if i == 0 {
                chunk.missing = missing.clone().into();
            }
            if i == last {
                chunk.caught_up = read_all && !self.caught_up;
                self.caught_up |= read_all;
            }
            let worth = !chunk.records.is_empty() || chunk.missing.is_set() || chunk.caught_up;
            if worth && !put(io, chunk).await {
                return false;
            }
        }
        true
    }
}

/// Resolves when the watched directory changed; never, without a watch.
async fn changed(wake: Option<&Wake>) -> std::io::Result<()> {
    match wake {
        Some(w) => w.changed().await,
        None => std::future::pending().await,
    }
}

async fn put(io: &Io, chunk: pb::ConsoleChunk) -> bool {
    io.outgoing.send(chunk.encode_to_vec()).await.is_ok()
}

async fn serve(dir: Option<PathBuf>, opened: Opened, mut io: Io) -> Result<(), Ended> {
    let asked = pb::ConsoleRead::decode_from_slice(&opened.params)
        .map_err(|e| ended("invalid", format!("the params are not a ConsoleRead: {e}")))?;
    let matcher = Matcher::new(&asked.pattern, asked.regex)
        .map_err(|e| ended("invalid", format!("pattern: {e}")))?;
    let dir =
        dir.ok_or_else(|| ended("no_stream", "the workload writes no console stream here"))?;
    let at = dir.clone();
    let next = match (asked.last, asked.since.as_option().map(nanos)) {
        (last, _) if last > 0 => {
            blocking(move || end_offset(&StreamDir::new(&at)).map(|e| e.saturating_sub(last)))
                .await?
        }
        (_, Some(since)) => blocking(move || offset_since(&StreamDir::new(&at), since)).await?,
        _ => asked.from_offset,
    };
    let left = (asked.limit > 0).then_some(asked.limit);
    // A follower that cannot watch, out of inotify watches say, still follows by looking.
    let mut wake = if asked.follow { Wake::watch(&dir).ok() } else { None };
    let mut reader = Reader { dir, matcher, next, left, caught_up: false };
    loop {
        let batch = reader.batch().await?;
        let more = batch.records.len() >= READ;
        if !reader.send(batch, &io).await || reader.left == Some(0) {
            return Ok(());
        }
        if more {
            continue;
        }
        if !asked.follow {
            return Ok(());
        }
        tokio::select! {
            () = tokio::time::sleep(FALLBACK) => {}
            heard = changed(wake.as_ref()) => if heard.is_err() {
                // A watch that fails is given up, and the follower looks every second instead.
                wake = None;
            },
            got = io.incoming.recv() => if got.is_none() { return Ok(()) },
        }
    }
}
