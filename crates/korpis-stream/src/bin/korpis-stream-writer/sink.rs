// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where the workload's bytes go: cut into lines per source, then written, or, while the stream
//! cannot be written at all (its quota full, its disk failing), dropped and counted, never held
//! and never refused, because the workload writes into a pipe and a pipe nobody reads blocks it.
//! The writer is reopened at each tick until it takes writes again, and its first record is the
//! gap that says what was lost meanwhile.

use crate::input::{Input, What};
use korpis_stream::changes::{self, Change};
use korpis_stream::dir::StreamDir;
use korpis_stream::lines::{Lines, Piece};
use korpis_stream::redact::Redactor;
use korpis_stream::writer::{Limits, StreamWriter};
use korpis_stream::{Kind, Source};

/// A line still unfinished after this long without bytes is written as it stands.
const PARTIAL_AFTER: i64 = 200_000_000;
const UNWRITABLE: &str = "the stream's storage refused writes (full, or failing)";
const UNREAD: &str = "console input the workload was not reading";

#[expect(clippy::print_stderr, reason = "the writer's only voice is its standard error")]
fn warn(what: &str, e: &std::io::Error) {
    eprintln!("korpis-stream-writer: {what}: {e}");
}

/// The writer's state.
pub struct Sink {
    dir: StreamDir,
    limits: Limits,
    redactor: Redactor,
    writer: Option<StreamWriter>,
    lines: [Lines; 2],
    last_bytes: [i64; 2],
    lost: Option<(u64, u64, i64, i64)>,
    pending: Vec<Change>,
}

const SOURCES: [Source; 2] = [Source::Stdout, Source::Stderr];

impl Sink {
    /// The stream in `dir`, opened if it can be.
    pub fn new(dir: StreamDir, limits: Limits, redactor: Redactor, now: i64) -> Self {
        let max = limits.max_record;
        let mut sink = Self {
            dir,
            limits,
            redactor,
            writer: None,
            lines: [Lines::new(max), Lines::new(max)],
            last_bytes: [now; 2],
            lost: None,
            pending: Vec::new(),
        };
        sink.reopen(now);
        sink
    }

    fn reopen(&mut self, now: i64) {
        match StreamWriter::open(self.dir.clone(), self.limits, self.redactor.clone(), now) {
            Ok(mut w) => {
                if let Some((records, bytes, first, last)) = self.lost.take()
                    && let Err(e) = w.lost((records, bytes), (first, last), UNWRITABLE)
                {
                    self.lost = Some((records, bytes, first, last));
                    warn("writing a gap", &e);
                    return;
                }
                self.writer = Some(w);
            }
            Err(e) => warn("opening the stream", &e),
        }
    }

    fn count_lost(&mut self, records: u64, bytes: u64, now: i64) {
        let l = self.lost.get_or_insert((0, 0, now, now));
        l.0 += records;
        l.1 += bytes;
        l.3 = now;
    }

    fn piece(&mut self, source: usize, piece: &Piece, now: i64) {
        let Some(w) = &mut self.writer else {
            let (records, bytes) = match piece {
                Piece::Line(b) => (1, u64::try_from(b.len()).unwrap_or(u64::MAX)),
                Piece::Dropped(n) => (0, *n),
            };
            return self.count_lost(records, bytes, now);
        };
        let written = match piece {
            Piece::Line(b) => {
                let src = SOURCES.get(source).cloned().unwrap_or(Source::Stdout);
                w.write(src, Kind::Text, b, now)
            }
            Piece::Dropped(n) => w.dropped_bytes(*n, now),
        };
        if let Err(e) = written {
            warn("writing", &e);
            self.writer = None;
            self.count_lost(1, 0, now);
        }
    }

    /// Takes in `bytes` from `source` (0 stdout, 1 stderr).
    pub fn push(&mut self, source: usize, bytes: &[u8], now: i64) {
        let Some(lines) = self.lines.get_mut(source) else { return };
        let pieces = lines.push(bytes);
        if let Some(at) = self.last_bytes.get_mut(source) {
            *at = now;
        }
        for p in pieces {
            self.piece(source, &p, now);
        }
    }

    /// Records `input` in the stream, its data redacted, before it reaches the workload.
    pub fn input(&mut self, input: &Input, now: i64) {
        let (kind, redacted) = match &input.what {
            What::Data(data) => (Kind::Structured, self.redactor.apply(data).into_owned()),
            What::Resize(..) => (Kind::Control, Vec::new()),
        };
        let body = crate::input::body(input, &redacted);
        let Some(w) = &mut self.writer else { return self.count_lost(1, 0, now) };
        if let Err(e) = w.write(Source::Input, kind, &body, now) {
            warn("writing input", &e);
            self.writer = None;
            self.count_lost(1, 0, now);
        }
    }

    /// Says that `bytes` of input were dropped because the workload is not reading its stdin.
    pub fn input_dropped(&mut self, bytes: u64, now: i64) {
        if let Some(w) = &mut self.writer
            && let Err(e) = w.lost((0, bytes), (now, now), UNREAD)
        {
            warn("writing a gap", &e);
        }
    }

    /// The periodic work.
    pub fn tick(&mut self, now: i64) {
        if self.writer.is_none() {
            self.reopen(now);
        }
        for source in 0..2 {
            let idle = self.last_bytes.get(source).is_some_and(|at| now - at >= PARTIAL_AFTER);
            let flushed =
                if idle { self.lines.get_mut(source).and_then(Lines::flush) } else { None };
            if let Some(p) = flushed {
                self.piece(source, &p, now);
            }
        }
        if let Some(w) = &mut self.writer
            && let Err(e) = w.tick(now)
        {
            warn("ticking", &e);
            self.writer = None;
        }
        self.record_changes();
    }

    fn record_changes(&mut self) {
        if let Some(w) = &mut self.writer {
            let c = w.changes();
            self.pending.extend(c.sealed.into_iter().map(Change::Sealed));
            self.pending.extend(c.pruned.into_iter().map(Change::Pruned));
        }
        match changes::append(self.dir.root(), &self.pending) {
            Ok(()) => self.pending.clear(),
            Err(e) => warn("recording changes", &e),
        }
    }

    /// The workload's output ended: what is held is written, and the segment sealed, so its last
    /// words are indexed.
    pub fn finish(&mut self, now: i64) {
        for source in 0..2 {
            if let Some(p) = self.lines.get_mut(source).and_then(Lines::flush) {
                self.piece(source, &p, now);
            }
        }
        self.tick(now);
        if let Some(w) = &mut self.writer
            && let Err(e) = w.seal()
        {
            warn("sealing", &e);
        }
        self.record_changes();
    }
}
