// SPDX-License-Identifier: AGPL-3.0-or-later

//! A stream continued on another node. A migrated workload's stream crosses with its volume, and
//! the source reports the offset its next record would have taken. Where fewer records arrived than
//! were written there, a sealed segment holding one gap record, at the offset before that one, says
//! how many were lost, so the writer, which continues after the last segment, starts at the offset
//! handed over: offsets never restart, and none is used twice. The seal is recorded in the changes
//! log as a writer's is, so the agent journals the segment.

use crate::changes::{self, Change};
use crate::dir::StreamDir;
use crate::record::{Gap, Kind, Record, Source};
use crate::segment::SegmentWriter;
use std::io;
use std::path::Path;

/// Why the records were lost, for a person reading the gap.
const REASON: &str = "written on the node the workload moved from, and not carried";

/// Continues the stream kept in `dir` at `next`, the offset its next record took on the node it was
/// written on, as of `now` (nanoseconds since the Unix epoch); whether a gap had to be written.
///
/// # Errors
/// What the filesystem refused.
pub fn continue_at(dir: &Path, next: u64, now: i64) -> io::Result<bool> {
    std::fs::create_dir_all(dir)?;
    let stream = StreamDir::new(dir);
    stream.settle()?;
    let held = crate::read::end_offset(&stream)?;
    let Some(at) = next.checked_sub(1).filter(|_| held < next) else { return Ok(false) };
    let gap = Gap { records: next - held, bytes: 0, millis: 0, reason: REASON.to_owned() };
    let record = Record {
        offset: at,
        time_nanos: now,
        source: Source::Korpis,
        kind: Kind::Gap,
        body: gap.body(),
    };
    let mut segment = SegmentWriter::create(&stream.open_path(at), at)?;
    segment.append(&record)?;
    let sealed = segment.seal()?;
    stream.mark_sealed(at)?;
    changes::append(dir, &[Change::Sealed(sealed)])?;
    Ok(true)
}

#[cfg(test)]
mod tests;
