// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading the journal back, to send it. What follows a cursor is read in order; where the
//! journal no longer holds a number the control plane has not ingested, a gap record is made
//! for it on the way out, so the control plane's cursor can pass the hole instead of waiting
//! for records nobody has. It names why: discarded, because the cursor is older than the
//! journal's tail (a control plane restored from a backup older than the retention window,
//! §8 of docs/03-state.md), or damaged, because the disk no longer reads them.

use super::segment::{self, HEADER};
use super::{Journal, ReadPos};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use std::path::PathBuf;

/// The cause of a gap the journal no longer holds because it was discarded.
pub const DISCARDED: &str = "discarded before the control plane ingested it";
/// The cause of a gap the journal no longer holds because its disk does not read it.
pub const DAMAGED: &str = "journal damaged";

/// The first number `r` stands for.
fn first(r: &agent::JournalRecord) -> u64 {
    match &r.body {
        Some(Body::Gap(g)) if g.first_seq <= r.seq => g.first_seq,
        _ => r.seq,
    }
}

/// A gap record for `first` through `last`.
fn gap(first: u64, last: u64, cause: &str) -> agent::JournalRecord {
    let gap = agent::JournalGap {
        first_seq: first,
        last_seq: last,
        cause: cause.to_owned(),
        ..Default::default()
    };
    agent::JournalRecord {
        seq: last,
        body: Some(Body::Gap(Box::new(gap))),
        count: 1,
        ..Default::default()
    }
}

/// What a read collects.
struct Collect {
    out: Vec<agent::JournalRecord>,
    expected: u64,
    bytes: u64,
    tail: u64,
    max: u64,
    pos: Option<ReadPos>,
}

impl Collect {
    /// Takes `r`, which ends at `end` in segment `segment`; whether to read on.
    fn take(&mut self, r: agent::JournalRecord, segment: u64, end: u64, len: u64) -> bool {
        if r.seq < self.expected {
            return true;
        }
        let start = first(&r);
        if start > self.expected {
            let cause = if self.expected < self.tail { DISCARDED } else { DAMAGED };
            self.out.push(gap(self.expected, start.saturating_sub(1), cause));
        }
        self.expected = r.seq.saturating_add(1);
        self.bytes = self.bytes.saturating_add(len);
        self.pos = Some(ReadPos { seq: r.seq, segment, offset: end });
        self.out.push(r);
        self.bytes < self.max
    }
}

impl Journal {
    /// The records after `after`, in order, up to about `max_bytes` of them, with a gap record
    /// standing for any number the journal no longer holds.
    pub fn read_after(
        &mut self,
        after: u64,
        max_bytes: u64,
    ) -> std::io::Result<Vec<agent::JournalRecord>> {
        if after >= self.head {
            return Ok(Vec::new());
        }
        let mut segments: Vec<(u64, PathBuf)> =
            self.sealed.iter().map(|s| (s.first, s.path.clone())).collect();
        if let Some(a) = &self.active {
            segments.push((a.first, a.path.clone()));
        }
        let expected = after.saturating_add(1);
        let start = segments.iter().rposition(|(f, _)| *f <= expected).unwrap_or(0);
        let resume = self.reader.filter(|r| r.seq == after).and_then(|r| {
            segments.iter().position(|(f, _)| *f == r.segment).map(|i| (i, r.offset))
        });
        let (mut index, mut offset) = resume.unwrap_or((start, 0));
        let tail = self.tail();
        let mut c =
            Collect { out: Vec::new(), expected, bytes: 0, tail, max: max_bytes, pos: None };
        while let Some((first, path)) = segments.get(index) {
            let mut last_end = offset.max(HEADER);
            segment::read(path, offset, |r, end| {
                let len = end.saturating_sub(last_end);
                last_end = end;
                c.take(r, *first, end, len)
            })?;
            if c.bytes >= max_bytes {
                break;
            }
            index = index.saturating_add(1);
            offset = 0;
        }
        if c.pos.is_some() {
            self.reader = c.pos;
        }
        Ok(c.out)
    }
}
