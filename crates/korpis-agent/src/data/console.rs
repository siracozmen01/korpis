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
//!
//! A session whose grant is limited to a range of the stream (§8 of docs/14-streams.md) reads
//! from the first record written at its start, sends only records written within it, and ends at
//! the first record written at or after its end, or, following, once the node's clock passes it
//! with nothing left to read. Its bounds are the node's clock, as each record states its time.
//!
//! A task or scheduled workload's runs each write their own console (§4 of docs/14-streams.md):
//! the params name the run, and the node looks for its stream only beneath the session's
//! workload.
//!
//! A read is an effect before it is anything else (§8 of docs/17-security.md): once the node has
//! found the stream, it journals that the session's subject read it, on this session, under the
//! grant its token was minted from, and sends nothing until the journal holds that. So a console
//! someone read is never one the journal does not say they read, and the control plane believes
//! it as it believes typed input, as far as that grant permitted the read then.

use super::handler::{Ended, Handler, Io, Opened, Serving};
use super::wake::Wake;
use crate::journal::JournalHandle;
use crate::reconcile::streams::{Located, stream_name};
use buffa::Message as _;
use buffa_types::google::protobuf::Timestamp;
use korpis_core::authz::StreamRange;
use korpis_core::principal::Principal;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::v1 as pb;
use korpis_stream::dir::StreamDir;
use korpis_stream::read::{Batch, end_offset, offset_since, read};
use korpis_stream::record::Record;
use korpis_stream::search::Matcher;
use korpis_wire::record::record_to_pb;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

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
    journal: JournalHandle,
}

impl ConsoleRead {
    /// Reads the streams `located` says where to find, journaling each read.
    #[must_use]
    pub const fn new(located: Located, journal: JournalHandle) -> Self {
        Self { located, journal }
    }
}

impl Handler for ConsoleRead {
    fn serve(&self, opened: Opened, io: Io) -> Serving {
        Box::pin(serve(self.clone(), opened, io))
    }
}

fn ended(code: &str, message: impl Into<String>) -> Ended {
    Ended { code: code.to_owned(), message: message.into() }
}

fn text(s: &str) -> pb::Value {
    pb::Value { kind: Some(pb::value::Kind::StringValue(s.to_owned())), ..Default::default() }
}

/// The effect of `opened`'s subject reading what `asked` asks for.
fn effect(opened: &Opened, actor: &Principal, asked: &pb::ConsoleRead) -> pb::Effect {
    let mut after = BTreeMap::from([("session".to_owned(), text(&opened.session))]);
    for (key, value) in [("run", &asked.run), ("pattern", &asked.pattern)] {
        if !value.is_empty() {
            after.insert(key.to_owned(), text(value));
        }
    }
    let follow =
        pb::Value { kind: Some(pb::value::Kind::BoolValue(asked.follow)), ..Default::default() };
    after.insert("follow".to_owned(), follow);
    pb::Effect {
        workload: Some(opened.workload.clone()),
        action: ACTION.to_owned(),
        outcome: pb::EffectOutcome::EFFECT_OUTCOME_SUCCEEDED.into(),
        actor: korpis_wire::principal::to_pb(actor).into(),
        grant: opened
            .chain
            .first()
            .map(|g| korpis_wire::wire_id(korpis_core::id::IdKind::Grant, *g)),
        after,
        ..Default::default()
    }
}

/// Journals that `opened`'s subject reads what `asked` asks for; the read goes no further if it
/// cannot be.
async fn journaled(
    journal: &JournalHandle,
    opened: &Opened,
    asked: &pb::ConsoleRead,
) -> Result<(), Ended> {
    let actor = Principal::from_key(&opened.subject)
        .map_err(|e| ended("refused", format!("the session's subject: {e}")))?;
    let e = effect(opened, &actor, asked);
    journal
        .append(Body::Effect(Box::new(e)))
        .await
        .map_err(|e| ended("unavailable", format!("the read could not be recorded: {e}")))?;
    Ok(())
}

fn nanos(t: &Timestamp) -> i64 {
    t.seconds.saturating_mul(1_000_000_000).saturating_add(i64::from(t.nanos))
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
    /// The range it may read, in nanoseconds; it is done once it has examined a record past it.
    range: Option<StreamRange>,
    done: bool,
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
            if self.range.is_some_and(|g| g.nanos().1 <= r.time_nanos) {
                self.done = true;
                break;
            }
            examined = r.offset + 1;
            let inside = self.range.is_none_or(|g| g.holds(r.time_nanos));
            if inside && self.matcher.keeps(&r) {
                self.left = self.left.map(|n| n - 1);
                kept.push(r);
            }
        }
        self.next = if self.left == Some(0) || self.done { examined } else { next };
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
            current.push(record_to_pb(r));
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

async fn serve(console: ConsoleRead, opened: Opened, mut io: Io) -> Result<(), Ended> {
    let asked = pb::ConsoleRead::decode_from_slice(&opened.params)
        .map_err(|e| ended("invalid", format!("the params are not a ConsoleRead: {e}")))?;
    let matcher = Matcher::new(&asked.pattern, asked.regex)
        .map_err(|e| ended("invalid", format!("pattern: {e}")))?;
    if asked.run.contains('/') {
        return Err(ended("invalid", "run: not a run"));
    }
    // A run's stream is found only beneath the session's workload, which is what it grants.
    let name = stream_name(&opened.workload, &asked.run);
    let dir = console.located.borrow().get(&name).cloned();
    let dir = dir.ok_or_else(|| ended("no_stream", "no such console stream is written here"))?;
    journaled(&console.journal, &opened, &asked).await?;
    let at = dir.clone();
    let next = match (asked.last, asked.since.as_option().map(nanos)) {
        (last, _) if last > 0 => {
            blocking(move || end_offset(&StreamDir::new(&at)).map(|e| e.saturating_sub(last)))
                .await?
        }
        (_, Some(since)) => blocking(move || offset_since(&StreamDir::new(&at), since)).await?,
        _ => asked.from_offset,
    };
    let range = opened.stream_range;
    let next = match range {
        Some(r) => {
            let (from, at) = (r.nanos().0, dir.clone());
            next.max(blocking(move || offset_since(&StreamDir::new(&at), from)).await?)
        }
        None => next,
    };
    let left = (asked.limit > 0).then_some(asked.limit);
    // A follower that cannot watch, out of inotify watches say, still follows by looking.
    let mut wake = if asked.follow { Wake::watch(&dir).ok() } else { None };
    let mut reader = Reader { dir, matcher, next, left, caught_up: false, range, done: false };
    loop {
        let batch = reader.batch().await?;
        let more = batch.records.len() >= READ;
        if !reader.send(batch, &io).await || reader.left == Some(0) || reader.done {
            return Ok(());
        }
        if more {
            continue;
        }
        // A record written just before the range ends may not be readable yet: a follower waits
        // one more look past it.
        if !asked.follow || range.is_some_and(|r| SystemTime::now() >= r.to + FALLBACK) {
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
