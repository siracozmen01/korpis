// SPDX-License-Identifier: AGPL-3.0-or-later

//! The watermarks the source reports at `quiesce`: for each stream of the workload, the offset its
//! next record takes, read from its segments once its writer sealed the last, and how far its
//! changes log was journaled. A stream whose last segment is still open is still being written,
//! since a writer seals as its input ends after the workload stopped, so the report waits for it.
//! On the destination, before the workload writes its stream there, the stream is continued at
//! the offset handed over and its changes log read from where the source journaled it to.

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

    /// On the destination, as the stream `stream` in `dir` is about to be written: continued at
    /// the offset `a`'s migration handed over for it, and its changes log read from where the
    /// source had journaled it to, so no record's offset is used twice and no segment journaled
    /// twice. Nothing for a stream no migration hands over.
    pub(in crate::reconcile) fn handed_over(
        &mut self,
        a: &agent::Assignment,
        stream: &str,
        dir: &Path,
    ) -> std::io::Result<()> {
        let Some(role) = a.migration.as_option() else { return Ok(()) };
        let here = role.role.as_known() == Some(agent::MigrationSide::MIGRATION_SIDE_DESTINATION);
        let Some(w) = role.watermarks.iter().find(|w| here && w.stream == stream) else {
            return Ok(());
        };
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let nanos = i64::try_from(now.as_nanos()).unwrap_or(i64::MAX);
        korpis_stream::handover::continue_at(dir, w.next_offset, nanos)?;
        let read =
            self.streams.watched().into_iter().find(|(n, _)| n == stream).map(|(_, s)| s.next);
        if read.is_some_and(|r| r < w.journaled_to) {
            self.streams.read_to(stream, w.journaled_to)?;
        }
        Ok(())
    }
}
