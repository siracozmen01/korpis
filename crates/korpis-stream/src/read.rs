// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a stream from an offset (§2 of docs/14-streams.md): whatever segments hold it, sealed
//! or still written, in offset order. Offsets a reader asks for that are no longer on disk,
//! pruned to the retention or never held here, are reported as such rather than skipped in
//! silence, so the reader can show them as the gap they are.

use crate::dir::StreamDir;
use crate::record::Record;
use crate::segment;
use std::io;

/// What a read found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// Offsets asked for that are not held here: from the offset asked to the first held.
    pub missing: Option<(u64, u64)>,
    /// The records, in offset order.
    pub records: Vec<Record>,
    /// The offset to ask for next.
    pub next: u64,
}

/// Up to `limit` records of the stream in `dir` from `from` on.
pub fn read(dir: &StreamDir, from: u64, limit: usize) -> io::Result<Batch> {
    let segments = dir.segments()?;
    let Some(first_held) = segments.first().map(|s| s.first_offset) else {
        return Ok(Batch { missing: None, records: Vec::new(), next: from });
    };
    let missing = (from < first_held).then_some((from, first_held));
    let start = from.max(first_held);
    // The segment holding `start` is the last one beginning at or before it.
    let begin = segments.iter().rposition(|s| s.first_offset <= start).unwrap_or(0);
    let mut records = Vec::new();
    for s in segments.iter().skip(begin) {
        if records.len() >= limit {
            break;
        }
        let (mut found, _) = segment::records_from(&s.path, start, limit - records.len())?;
        records.append(&mut found);
    }
    let next = records.last().map_or(start, |r| r.offset + 1);
    Ok(Batch { missing, records, next })
}

/// One past the last record of the stream in `dir`: where a reader that wants only what comes
/// next starts.
pub fn end_offset(dir: &StreamDir) -> io::Result<u64> {
    let Some(last) = dir.segments()?.pop() else { return Ok(0) };
    if last.sealed
        && let Some(info) = segment::sealed_info(&last.path)?
    {
        return Ok(info.end_offset);
    }
    Ok(segment::scan(&last.path)?.end_offset)
}

/// The offset of the first record, in offset order, written at or after `since` (nanoseconds
/// since the Unix epoch, node clock), in the stream in `dir`; one past its last record if none
/// was. A sealed segment whose last record is older is skipped by its footer alone.
pub fn offset_since(dir: &StreamDir, since: i64) -> io::Result<u64> {
    let mut end = 0;
    for s in dir.segments()? {
        if s.sealed
            && let Some(info) = segment::sealed_info(&s.path)?
            && info.times.1 < since
        {
            end = info.end_offset;
            continue;
        }
        let (records, _) = segment::records_from(&s.path, s.first_offset, usize::MAX)?;
        if let Some(r) = records.iter().find(|r| r.time_nanos >= since) {
            return Ok(r.offset);
        }
        end = records.last().map_or(end.max(s.first_offset), |r| r.offset + 1);
    }
    Ok(end)
}
