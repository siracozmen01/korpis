// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node journal (§4.10 of docs/02-architecture.md): every record the node originates,
//! numbered by a sequence that is never reused and survives a restart, written durably to an
//! append-only log of segment files before the append returns. Nothing is discarded before the
//! control plane acknowledges it, and acknowledged records are kept for a retention window, so
//! a control plane restored from an older backup can ingest them again. The journal is bounded
//! and the bound is declared: repeats are coalesced first, acknowledged records past their
//! window and then within it are discarded next, and only then are records lost, each run of
//! them recorded by a gap record naming its numbers. A workload is never stopped to keep the
//! journal.

mod coalesce;
mod handle;
mod meta;
mod read;
mod segment;
mod ship;
mod space;

pub use handle::{JournalHandle, Usage};
pub use ship::Shipper;

use buffa_types::google::protobuf::Timestamp;
use coalesce::{Coalescer, Offer, Pending};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use meta::Meta;
use segment::{Active, Ending, HEADER};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The cause a gap names when the journal reached its bound (§2 of docs/15-observability.md).
pub const EXHAUSTED: &str = "journal exhausted";
/// What is kept free for the gap record that reports a loss.
const GAP_RESERVE: u64 = 4096;

/// The journal's declared bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// The most the journal holds on disk, retained records included.
    pub bytes: u64,
    /// How large a segment grows before the next one begins: the unit of discarding.
    pub segment: u64,
    /// How long acknowledged records are kept.
    pub retention: Duration,
    /// How long repeats of an effect are counted before they are written.
    pub coalesce: Duration,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            bytes: 2 << 30,
            segment: 16 << 20,
            retention: Duration::from_secs(7 * 24 * 3600),
            coalesce: Duration::from_secs(10),
        }
    }
}

/// What became of a record appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appended {
    /// It is on disk with this number.
    Written(u64),
    /// It repeats the effect before it and is counted, to be written with the run.
    Coalesced,
    /// The journal is at its bound: this number is lost, and a gap record will name it.
    Lost(u64),
}

/// A sealed segment: no longer appended to.
#[derive(Debug, Clone)]
struct Sealed {
    first: u64,
    path: PathBuf,
    bytes: u64,
}

/// Where the last read stopped, so the next one resumes there instead of rereading a segment.
#[derive(Debug, Clone, Copy)]
struct ReadPos {
    seq: u64,
    segment: u64,
    offset: u64,
}

/// The journal.
#[derive(Debug)]
pub struct Journal {
    dir: PathBuf,
    bounds: Bounds,
    sealed: Vec<Sealed>,
    active: Option<Active>,
    head: u64,
    meta: Meta,
    coalescer: Coalescer,
    reader: Option<ReadPos>,
    appended: u64,
}

fn timestamp(t: SystemTime) -> Timestamp {
    let since = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    Timestamp {
        seconds: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(since.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    }
}

fn invalid(why: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, why)
}

/// The highest sequence number in the segment at `path`, how far it reads as whole frames, and
/// how it ended.
fn scan(path: &Path) -> std::io::Result<(Option<u64>, u64, Ending)> {
    let mut last = None;
    let (ending, len) = segment::read(path, 0, |r, _| {
        last = Some(r.seq);
        true
    })?;
    Ok((last, len, ending))
}

impl Journal {
    /// The journal kept in `dir`, created if there is none. A record cut short by a crash at
    /// the end of the last segment is cut away; everything before it is kept.
    pub fn open(dir: &Path, bounds: Bounds) -> std::io::Result<Self> {
        let floor = bounds.segment.saturating_mul(2).saturating_add(GAP_RESERVE);
        if bounds.segment < 64 << 10 || bounds.bytes < floor {
            return Err(invalid(format!("journal bounds {bounds:?}: at least two segments")));
        }
        std::fs::create_dir_all(dir)?;
        let meta = Meta::load(&dir.join("state")).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "the journal's state does not read; starting it afresh");
            Meta::default()
        });
        let mut firsts: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter_map(|p| segment::first_of(&p).map(|f| (f, p)))
            .collect();
        firsts.sort();
        let mut journal = Self {
            dir: dir.to_owned(),
            bounds,
            sealed: Vec::new(),
            active: None,
            head: 0,
            meta,
            coalescer: Coalescer::new(bounds.coalesce),
            reader: None,
            appended: 0,
        };
        if let Some((first, path)) = firsts.pop() {
            journal.resume(first, &path, firsts)?;
        }
        Ok(journal)
    }

    /// Takes up the last segment as the active one, and the others as sealed.
    fn resume(
        &mut self,
        first: u64,
        path: &Path,
        older: Vec<(u64, PathBuf)>,
    ) -> std::io::Result<()> {
        let (last, len, ending) = scan(path)?;
        if ending != Ending::Clean {
            tracing::warn!(segment = %path.display(), ?ending, len, "journal tail cut back");
        }
        self.active = Some(Active::reopen(path, first, len.max(HEADER), last.is_some())?);
        for (first, path) in older {
            let bytes = std::fs::metadata(&path)?.len();
            self.sealed.push(Sealed { first, path, bytes });
        }
        self.head = match last {
            Some(seq) => seq,
            None => self.last_sealed_seq()?,
        };
        Ok(())
    }

    /// The highest number in the sealed segments, newest first: for an active segment begun
    /// and left empty by a crash.
    fn last_sealed_seq(&self) -> std::io::Result<u64> {
        for s in self.sealed.iter().rev() {
            if let (Some(seq), _, _) = scan(&s.path)? {
                return Ok(seq);
            }
        }
        Ok(0)
    }

    /// The highest number written.
    #[must_use]
    pub fn head(&self) -> u64 {
        self.head
    }

    /// The lowest number the journal may still hold: the first of its oldest segment, or zero
    /// if it holds nothing.
    #[must_use]
    pub fn tail(&self) -> u64 {
        let oldest = self.sealed.first().map(|s| s.first);
        match oldest.or_else(|| self.active.as_ref().filter(|a| a.holds).map(|a| a.first)) {
            Some(first) => first.min(self.head).max(1),
            None => 0,
        }
    }

    /// The bytes on disk.
    #[must_use]
    pub fn used(&self) -> u64 {
        let sealed: u64 = self.sealed.iter().map(|s| s.bytes).sum();
        sealed.saturating_add(self.active.as_ref().map_or(0, |a| a.len))
    }

    /// The next number to give a record.
    fn next_seq(&self) -> u64 {
        let lost = self.meta.lost.map_or(0, |(_, last)| last);
        self.head.max(self.meta.floor).max(lost).saturating_add(1)
    }

    /// Appends `body`, which happened at `now`. Unless it is coalesced or lost, it is on disk
    /// when this returns.
    pub fn append(&mut self, body: &Body, now: SystemTime) -> std::io::Result<Appended> {
        let flush = match self.coalescer.offer(body, now) {
            Offer::Held { flush } => {
                if let Some(p) = flush {
                    self.write_pending(p, now)?;
                }
                return Ok(Appended::Coalesced);
            }
            Offer::Write { flush } => flush,
        };
        if let Some(p) = flush {
            self.write_pending(p, now)?;
        }
        let appended = self.place(body, timestamp(now), 1, now)?;
        if matches!(appended, Appended::Written(_)) {
            self.coalescer.written(body, now);
        }
        Ok(appended)
    }

    /// Writes counted repeats as one record.
    fn write_pending(&mut self, p: Pending, now: SystemTime) -> std::io::Result<()> {
        let Pending { body, first_at, count } = p;
        if let Appended::Written(_) = self.place(&body, timestamp(first_at), count, now)? {
            self.coalescer.written(&body, now);
        }
        Ok(())
    }

    /// Writes a record if the bound leaves room for it, making room first if it can; loses its
    /// number otherwise.
    fn place(
        &mut self,
        body: &Body,
        at: Timestamp,
        count: u64,
        now: SystemTime,
    ) -> std::io::Result<Appended> {
        let seq = self.next_seq();
        let record = agent::JournalRecord {
            seq,
            occurred_at: at.into(),
            body: Some(body.clone()),
            count,
            ..Default::default()
        };
        let frame = segment::frame(&record)?;
        let need = u64::try_from(frame.len()).unwrap_or(u64::MAX);
        if !self.room(need, GAP_RESERVE) {
            self.reclaim(need, GAP_RESERVE)?;
        }
        if self.room(need, GAP_RESERVE) {
            self.flush_loss(now)?;
        }
        if self.meta.lost.is_some() || !self.room(need, GAP_RESERVE) {
            return self.lose(seq);
        }
        self.write(seq, &frame, now)?;
        Ok(Appended::Written(seq))
    }

    /// Whether `need` more bytes fit below the bound, keeping `reserve` free.
    fn room(&self, need: u64, reserve: u64) -> bool {
        let after = self.used().saturating_add(need).saturating_add(HEADER);
        after.saturating_add(reserve) <= self.bounds.bytes
    }

    /// Writes a framed record numbered `seq`, beginning a new segment if the active one is full.
    fn write(&mut self, seq: u64, frame: &[u8], now: SystemTime) -> std::io::Result<()> {
        let need = u64::try_from(frame.len()).unwrap_or(u64::MAX);
        let full = self
            .active
            .as_ref()
            .is_none_or(|a| a.holds && a.len.saturating_add(need) > self.bounds.segment);
        if full {
            if let Some(a) = self.active.take() {
                self.sealed.push(Sealed { first: a.first, path: a.path, bytes: a.len });
            }
            self.active = Some(Active::create(&self.dir, seq)?);
            // The segment just sealed may already be wholly acknowledged.
            if self.note_acknowledged(now) {
                self.meta.save(&self.dir.join("state"))?;
            }
        }
        let active = self.active.as_mut().ok_or_else(|| invalid("no segment".into()))?;
        active.append(frame)?;
        self.head = seq;
        self.appended = self.appended.saturating_add(need);
        Ok(())
    }

    /// Loses `seq` to the bound: it is remembered, durably, for the gap record.
    fn lose(&mut self, seq: u64) -> std::io::Result<Appended> {
        let before = self.meta.clone();
        self.meta.lost = Some((self.meta.lost.map_or(seq, |(first, _)| first), seq));
        self.meta.floor = self.meta.floor.max(seq);
        if let Err(e) = self.meta.save(&self.dir.join("state")) {
            self.meta = before;
            return Err(e);
        }
        Ok(Appended::Lost(seq))
    }

    /// Writes the gap record for the numbers lost, if there are any and it fits.
    fn flush_loss(&mut self, now: SystemTime) -> std::io::Result<()> {
        let Some((first, last)) = self.meta.lost else { return Ok(()) };
        let gap = agent::JournalGap {
            first_seq: first,
            last_seq: last,
            cause: EXHAUSTED.to_owned(),
            ..Default::default()
        };
        let record = agent::JournalRecord {
            seq: last,
            occurred_at: timestamp(now).into(),
            body: Some(Body::Gap(Box::new(gap))),
            count: 1,
            ..Default::default()
        };
        let frame = segment::frame(&record)?;
        if !self.room(u64::try_from(frame.len()).unwrap_or(u64::MAX), 0) {
            return Ok(());
        }
        self.write(last, &frame, now)?;
        self.meta.lost = None;
        self.meta.save(&self.dir.join("state"))
    }

    /// Writes counted repeats that are due, whatever is due of retention, and a pending gap.
    pub fn maintain(&mut self, now: SystemTime) -> std::io::Result<()> {
        if let Some(p) = self.coalescer.due(now) {
            self.write_pending(p, now)?;
        }
        self.discard_expired(now)?;
        if self.meta.lost.is_some() {
            self.reclaim(GAP_RESERVE, 0)?;
            self.flush_loss(now)?;
        }
        Ok(())
    }

    /// Writes every repeat still counted: for a clean shutdown.
    pub fn close(&mut self, now: SystemTime) -> std::io::Result<()> {
        match self.coalescer.drain() {
            Some(p) => self.write_pending(p, now),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
