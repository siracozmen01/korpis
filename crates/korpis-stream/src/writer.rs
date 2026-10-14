// SPDX-License-Identifier: AGPL-3.0-or-later

//! Writing a stream (§3, §5, §7 of docs/14-streams.md). Every record is redacted, then cut to the
//! largest record, in that order: cutting first could keep half a secret, where cutting after can
//! only shorten a redaction mark. Then it is admitted by the stream's rate (a token bucket in
//! bytes) or dropped. Drops are counted, and the next record admitted, or the next tick a second
//! after the last drop, writes a gap record saying how many records and bytes were lost, over how
//! long, and why. Nothing here ever waits for room: a workload is never blocked on its own output.
//!
//! The segment being written is sealed once it reaches its size or its age, and a sealed one is
//! reported to the caller, which journals it for the control plane's index (§4). Records reach
//! the page cache as they are written and the disk at each tick and seal: a crash of this
//! process loses nothing written, and a crash of the machine loses at most the last tick's.

use crate::dir::StreamDir;
use crate::record::{Gap, Kind, Record, Source};
use crate::redact::Redactor;
use crate::segment::{Sealed, SegmentWriter};
use std::io;
use std::time::Duration;

/// A stream's limits, as its workload declares them (§5 of docs/14-streams.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes per second admitted on average.
    pub rate: u64,
    /// Bytes admitted at once after a quiet period.
    pub burst: u64,
    /// The largest record kept; the rest of a longer one is dropped (question 5 of §9).
    pub max_record: usize,
    /// A segment is sealed once it holds this many bytes.
    pub segment_bytes: u64,
    /// Or once its first record is this old.
    pub segment_age: Duration,
    /// How much of the stream is kept.
    pub retention: Retention,
}

/// How much of a stream is kept, as its workload declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// Sealed segments beyond this many bytes are pruned, oldest first.
    Bytes(u64),
    /// Sealed segments whose last record is older than this are pruned.
    Age(Duration),
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            rate: 256 * 1024,
            burst: 4 * 1024 * 1024,
            max_record: 64 * 1024,
            segment_bytes: 8 * 1024 * 1024,
            segment_age: Duration::from_secs(300),
            retention: Retention::Bytes(64 * 1024 * 1024),
        }
    }
}

/// Records and bytes dropped since the last gap record.
#[derive(Debug, Clone, Copy)]
struct Dropped {
    records: u64,
    bytes: u64,
    first: i64,
    last: i64,
    reason: &'static str,
}

/// What became of the segments while writing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Segments sealed.
    pub sealed: Vec<Sealed>,
    /// Sealed segments pruned to the retention.
    pub pruned: Vec<Sealed>,
}

/// A stream's writer.
#[derive(Debug)]
pub struct StreamWriter {
    dir: StreamDir,
    limits: Limits,
    redactor: Redactor,
    active: Option<SegmentWriter>,
    next_offset: u64,
    tokens: u64,
    refilled_at: i64,
    dropped: Option<Dropped>,
    changes: Changes,
}

const RATE_EXCEEDED: &str = "write rate exceeded the stream's limit";
const TOO_LONG: &str = "record longer than the stream's largest record";

impl StreamWriter {
    /// The stream kept in `dir`, created if absent, continuing from what it holds: a segment a
    /// crash left half-written is cut back to its last whole record and written on.
    pub fn open(dir: StreamDir, limits: Limits, redactor: Redactor, now: i64) -> io::Result<Self> {
        std::fs::create_dir_all(dir.root())?;
        let (active, next_offset) = match dir.settle()? {
            Some((writing, _)) => {
                let w = SegmentWriter::recover(&writing.path)?;
                let end = w.end_offset();
                (Some(w), end)
            }
            None => (None, last_sealed_end(&dir)?),
        };
        Ok(Self {
            dir,
            limits,
            redactor,
            active,
            next_offset,
            tokens: limits.burst,
            refilled_at: now,
            dropped: None,
            changes: Changes::default(),
        })
    }

    /// The offset the next record will have.
    #[must_use]
    pub const fn next_offset(&self) -> u64 {
        self.next_offset
    }

    /// Writes one record of `source` at `now`, or drops it and counts the drop.
    pub fn write(&mut self, source: Source, kind: Kind, body: &[u8], now: i64) -> io::Result<()> {
        self.refill(now);
        let body = self.redactor.apply(body);
        let (kept, cut) = match body.get(..self.limits.max_record) {
            Some(kept) if body.len() > self.limits.max_record => (kept, body.len() - kept.len()),
            _ => (&body[..], 0),
        };
        let cost = u64::try_from(kept.len()).unwrap_or(u64::MAX).max(1);
        if cost > self.tokens {
            self.drop_bytes(1, cost + u64::try_from(cut).unwrap_or(u64::MAX), now, RATE_EXCEEDED);
            return Ok(());
        }
        self.tokens -= cost;
        self.flush_gap()?;
        self.append(source, kind, kept.to_vec(), now)?;
        if cut > 0 {
            self.drop_bytes(0, u64::try_from(cut).unwrap_or(u64::MAX), now, TOO_LONG);
            self.flush_gap()?;
        }
        Ok(())
    }

    /// Counts `records` and `bytes` lost between `first` and `last` before they reached here, for
    /// `reason`: what the caller dropped while it could not write at all. The gap record is
    /// written now.
    pub fn lost(
        &mut self,
        (records, bytes): (u64, u64),
        (first, last): (i64, i64),
        reason: &'static str,
    ) -> io::Result<()> {
        self.flush_gap()?;
        self.dropped = Some(Dropped { records, bytes, first, last, reason });
        self.flush_gap()
    }

    /// Counts `bytes` of an overlong record dropped before they reached here, at `now`.
    pub fn dropped_bytes(&mut self, bytes: u64, now: i64) -> io::Result<()> {
        self.drop_bytes(0, bytes, now, TOO_LONG);
        self.flush_gap()
    }

    /// The periodic work: a gap record a second after the last drop, a seal by age, and making
    /// what was written durable.
    pub fn tick(&mut self, now: i64) -> io::Result<()> {
        if self.dropped.is_some_and(|d| now - d.last >= 1_000_000_000) {
            self.flush_gap()?;
        }
        let age = i64::try_from(self.limits.segment_age.as_nanos()).unwrap_or(i64::MAX);
        let due = self
            .active
            .as_ref()
            .and_then(SegmentWriter::times)
            .is_some_and(|(f, _)| now - f >= age);
        if due {
            self.seal()?;
        }
        self.prune(Some(now))?;
        if let Some(active) = &self.active {
            active.sync()?;
        }
        Ok(())
    }

    /// Seals the segment being written, if it holds a record.
    pub fn seal(&mut self) -> io::Result<()> {
        let Some(active) = self.active.take() else { return Ok(()) };
        if active.end_offset() == active.first_offset() {
            self.active = Some(active);
            return Ok(());
        }
        let first = active.first_offset();
        let sealed = active.seal()?;
        self.dir.mark_sealed(first)?;
        self.changes.sealed.push(sealed);
        self.prune(None)
    }

    /// Prunes sealed segments to the retention, as of `now` for a retention by age.
    fn prune(&mut self, now: Option<i64>) -> io::Result<()> {
        let pruned = match (self.limits.retention, now) {
            (Retention::Bytes(max), _) => self.dir.prune_to(max)?,
            (Retention::Age(age), Some(now)) => {
                let age = i64::try_from(age.as_nanos()).unwrap_or(i64::MAX);
                self.dir.prune_before(now.saturating_sub(age))?
            }
            (Retention::Age(_), None) => Vec::new(),
        };
        self.changes.pruned.extend(pruned);
        Ok(())
    }

    /// What became of the segments since the last call.
    pub fn changes(&mut self) -> Changes {
        std::mem::take(&mut self.changes)
    }

    fn refill(&mut self, now: i64) {
        let elapsed = u64::try_from(now.saturating_sub(self.refilled_at)).unwrap_or(0);
        let earned = u128::from(elapsed) * u128::from(self.limits.rate) / 1_000_000_000;
        if earned > 0 {
            let earned = u64::try_from(earned).unwrap_or(u64::MAX);
            self.tokens = self.tokens.saturating_add(earned).min(self.limits.burst);
            self.refilled_at = now;
        }
    }

    fn drop_bytes(&mut self, records: u64, bytes: u64, now: i64, reason: &'static str) {
        let d = self.dropped.get_or_insert(Dropped {
            records: 0,
            bytes: 0,
            first: now,
            last: now,
            reason,
        });
        d.records += records;
        d.bytes += bytes;
        d.last = now;
    }

    fn flush_gap(&mut self) -> io::Result<()> {
        let Some(d) = self.dropped.take() else { return Ok(()) };
        let millis = u64::try_from((d.last - d.first) / 1_000_000).unwrap_or(0);
        let gap = Gap { records: d.records, bytes: d.bytes, millis, reason: d.reason.to_owned() };
        self.append(Source::Korpis, Kind::Gap, gap.body(), d.last)
    }

    fn append(&mut self, source: Source, kind: Kind, body: Vec<u8>, now: i64) -> io::Result<()> {
        if self.active.is_none() {
            self.active = Some(SegmentWriter::create(
                &self.dir.open_path(self.next_offset),
                self.next_offset,
            )?);
        }
        let record = Record { offset: self.next_offset, time_nanos: now, source, kind, body };
        if let Some(active) = &mut self.active {
            active.append(&record)?;
        }
        self.next_offset += 1;
        if self.active.as_ref().is_some_and(|a| a.bytes() >= self.limits.segment_bytes) {
            self.seal()?;
        }
        Ok(())
    }
}

/// One past the last offset of the newest sealed segment; 0 for a new stream.
fn last_sealed_end(dir: &StreamDir) -> io::Result<u64> {
    let Some(last) = dir.segments()?.into_iter().rfind(|s| s.sealed) else {
        return Ok(0);
    };
    Ok(crate::segment::scan(&last.path)?.end_offset)
}

#[cfg(test)]
mod tests;
