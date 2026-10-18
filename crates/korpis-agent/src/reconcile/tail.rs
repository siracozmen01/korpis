// SPDX-License-Identifier: AGPL-3.0-or-later

//! Journaling what became of each stream's segments (§4 of docs/14-streams.md). The writer
//! records every seal and prune in its stream's changes log; on every pass the reconciler reads
//! each log from where it left off and journals each change, from which the control plane keeps
//! its segment index. A stream whose workload left the set is read one last time and then no
//! longer watched; what it wrote stays on the node, as a volume does (§4 of docs/14-streams.md).

use super::Reconciler;
use super::streams::name_parts;
use crate::held::HeldSet;
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_core::workload::stream::Segment;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::v1 as pb;
use korpis_stream::changes::{self, Change};
use korpis_stream::segment::Sealed;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A node-clock time in nanoseconds since the Unix epoch; the epoch itself if it is before it.
fn at(nanos: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(u64::try_from(nanos).unwrap_or(0))
}

/// The journal record of `change` to the console stream named `name`.
fn record(name: &str, change: &Change) -> Body {
    let (event, s) = match change {
        Change::Sealed(s) => (agent::SegmentEvent::SEGMENT_EVENT_SEALED, s),
        Change::Pruned(s) => (agent::SegmentEvent::SEGMENT_EVENT_PRUNED, s),
    };
    segment_record(name, event, s, "")
}

/// The journal record of `event` to segment `s` of the console stream named `name`, a
/// workload's or one of its runs'; `repository` is where an offloaded one went, `rep_…`, and
/// empty for any other event.
pub(crate) fn segment_record(
    name: &str,
    event: agent::SegmentEvent,
    s: &Sealed,
    repository: &str,
) -> Body {
    let segment = Segment {
        first_offset: s.first_offset,
        end_offset: s.end_offset,
        first_at: at(s.times.0),
        last_at: at(s.times.1),
        bytes: s.bytes,
        digest: s.digest.clone(),
        node: None,
        repository: None,
    };
    let mut segment = korpis_wire::stream::segment_to_pb(&segment);
    if event == agent::SegmentEvent::SEGMENT_EVENT_OFFLOADED {
        segment.offloaded = true;
        repository.clone_into(&mut segment.repository);
    }
    let (workload, run) = name_parts(name);
    Body::StreamSegment(Box::new(agent::StreamSegmentEvent {
        workload: workload.to_owned(),
        run: run.to_owned(),
        kind: pb::StreamKind::STREAM_KIND_CONSOLE.into(),
        event: event.into(),
        segment: segment.into(),
        ..Default::default()
    }))
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Journals what every watched stream's writer recorded since the last pass, and stops
    /// watching a stream whose workload `held` no longer claims once it is read through. A run's
    /// stream is watched as long as its workload is, after its object is gone too: it is located
    /// by being watched, and a finished run's console is read by naming the run.
    pub(super) async fn tail(&mut self, held: &HeldSet) {
        for (name, watched) in self.streams.watched() {
            let read = self.journal_changes(&name, &watched.dir, watched.next).await;
            let kept = match read {
                Ok(next) => self.streams.read_to(&name, next),
                Err(e) => {
                    tracing::warn!(stream = %name, error = %e, "a stream's changes were not journaled");
                    continue;
                }
            };
            let gone = !held.assignments.contains_key(name_parts(&name).0);
            if let Err(e) =
                kept.and_then(|()| if gone { self.streams.unwatch(&name) } else { Ok(()) })
            {
                tracing::error!(stream = %name, error = %e, "the streams watched could not be kept");
            }
        }
    }

    /// Journals what is left of `workload`'s console stream's changes log and stops watching
    /// it: its volume is about to be deleted, and a change journaled after the deletion would
    /// name a segment that is gone as held here.
    pub(super) async fn finish_stream(&mut self, workload: &str) -> std::io::Result<()> {
        let Some((_, watched)) = self.streams.watched().into_iter().find(|(w, _)| w == workload)
        else {
            return Ok(());
        };
        let next = self.journal_changes(workload, &watched.dir, watched.next).await?;
        self.streams.read_to(workload, next)?;
        self.streams.unwatch(workload)
    }

    /// Journals the changes in the log in `dir` from byte `from`; the byte past the last one
    /// journaled. A record the journal refuses ends the pass, and the log is read again from
    /// `from` the next time: a change journaled twice is the same segment to the index.
    async fn journal_changes(&self, name: &str, dir: &Path, from: u64) -> std::io::Result<u64> {
        let mut done = from;
        for (change, next) in changes::read_from(dir, from)? {
            self.wiring.journal.append(record(name, &change)).await?;
            done = next;
        }
        Ok(done)
    }
}
