// SPDX-License-Identifier: AGPL-3.0-or-later

//! The watermarks the source reports at `quiesce`: for each stream of the workload, the offset its
//! next record takes, read from its segments once its writer sealed the last, and how far its
//! changes log was journaled. A stream whose last segment is still open is still being written,
//! since a writer seals as its input ends after the workload stopped, so the report waits for it.

use super::super::Reconciler;
use super::super::streams::name_parts;
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_stream::dir::StreamDir;
use std::path::Path;

/// The offset the next record of the stream kept in `dir` takes, once its last segment is sealed;
/// `None` while it is still written.
fn next_offset(dir: &Path) -> std::io::Result<Option<u64>> {
    let dir = StreamDir::new(dir);
    let segments = match dir.segments() {
        Ok(s) => s,
        // A stream nothing was ever written to has no directory yet.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(0)),
        Err(e) => return Err(e),
    };
    let Some(last) = segments.into_iter().next_back() else { return Ok(Some(0)) };
    if !last.sealed {
        return Ok(None);
    }
    Ok(Some(korpis_stream::segment::scan(&last.path)?.end_offset))
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Each of `workload`'s streams' watermarks, once every one of them is sealed.
    pub(super) fn watermarks(&self, workload: &str) -> Option<Vec<agent::StreamWatermark>> {
        let mut marks = Vec::new();
        for (stream, watched) in self.streams.watched() {
            if name_parts(&stream).0 != workload {
                continue;
            }
            let next = match next_offset(&watched.dir) {
                Ok(next) => next?,
                Err(e) => {
                    tracing::warn!(%stream, error = %e, "a stream's end could not be read");
                    return None;
                }
            };
            marks.push(agent::StreamWatermark {
                stream,
                next_offset: next,
                journaled_to: watched.next,
                ..Default::default()
            });
        }
        Some(marks)
    }
}
