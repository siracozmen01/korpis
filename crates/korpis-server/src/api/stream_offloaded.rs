// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a stream's offloaded segments through the control plane (§4 of docs/14-streams.md):
//! the use case decides who may, records the read, and grants the workload's key with minutes of
//! read access to its stream prefix in each repository holding a segment; this fetches each
//! segment, checks it is the one the index names, and sends its records as `ConsoleChunk`s, in
//! offset order. Offsets between the segments, which no offloaded segment holds, are sent as
//! missing; offsets after the last are not offloaded yet, and the last chunk says where that is.

use super::error;
use connectrpc::ConnectError;
use korpis_app::backup::offload::Offloaded;
use korpis_backup::Repo;
use korpis_backup::offload::segment_id;
use korpis_backup::s3::S3Store;
use korpis_core::authz::StreamRange;
use korpis_core::workload::stream::Segment;
use korpis_proto::korpis::v1 as pb;
use korpis_stream::record::Record;
use korpis_wire::record::record_to_pb;
use std::collections::BTreeMap;
use uuid::Uuid;

/// What is left to send.
struct Reading {
    segments: std::vec::IntoIter<(Segment, Uuid)>,
    repos: BTreeMap<Uuid, Repo<S3Store>>,
    next: u64,
    left: u64,
    /// The range the caller may read: records outside it are never sent, a segment wholly before
    /// it is passed over without being fetched, and the reading ends at the first past it.
    range: Option<StreamRange>,
}

/// The repositories `read` reaches, opened.
pub(super) fn opened(read: &Offloaded) -> Result<BTreeMap<Uuid, Repo<S3Store>>, ConnectError> {
    let mut repos = BTreeMap::new();
    for (id, grant) in &read.grants {
        let repo = crate::repos::repo_of(grant).map_err(|e| unavailable(&e))?;
        repos.insert(*id, repo);
    }
    Ok(repos)
}

fn unavailable(e: &dyn std::fmt::Display) -> ConnectError {
    let detail = e.to_string();
    error::from_app(korpis_app::AppError::Unavailable { what: "the stream's repository", detail })
}

/// The records of segment `s` in `repo` from `from`, at most `left` of them, checked against the
/// digest the index holds.
async fn fetched(
    repo: &Repo<S3Store>,
    s: &Segment,
    (from, left): (u64, u64),
) -> Result<Vec<Record>, ConnectError> {
    let id = segment_id(s.first_offset, &s.digest).ok_or_else(|| {
        error::altered("stream.segment_altered", "a segment's digest is malformed")
    })?;
    let bytes = repo.segment(&id).await.map_err(|e| match e {
        // Missing, or not what was sealed under that name: the store's doing, or the node's.
        korpis_backup::Error::Seal { .. } | korpis_backup::Error::Corrupt(_) => {
            error::altered("stream.segment_altered", &e.to_string())
        }
        other => unavailable(&other),
    })?;
    let digest = s.digest.clone();
    let limit = usize::try_from(left).unwrap_or(usize::MAX);
    let read = tokio::task::spawn_blocking(move || {
        korpis_stream::offloaded::records(&bytes, &digest, (from, limit))
    });
    let records = read.await.map_err(|e| error::internal(&e))?;
    records.map_err(|e| error::altered("stream.segment_altered", &e.to_string()))
}

impl Reading {
    /// The next chunk: one segment's records, after the offsets before it no segment holds.
    async fn chunk(&mut self) -> Option<Result<pb::ConsoleChunk, ConnectError>> {
        if self.left == 0 {
            return None;
        }
        let range = self.range;
        let mut found = self.segments.next();
        while let (Some(r), Some((s, _))) = (range, &found)
            && s.last_at < r.from
        {
            self.next = s.end_offset;
            found = self.segments.next();
        }
        let found = found.filter(|(s, _)| range.is_none_or(|r| s.first_at < r.to));
        let Some((s, repository)) = found else {
            self.left = 0;
            let last =
                pb::ConsoleChunk { next_offset: self.next, caught_up: true, ..Default::default() };
            return Some(Ok(last));
        };
        let repo = self.repos.get(&repository)?;
        let missing = (s.first_offset > self.next).then(|| pb::OffsetRange {
            first: self.next,
            end: s.first_offset,
            ..Default::default()
        });
        let mut records = match fetched(repo, &s, (self.next, self.left)).await {
            Ok(records) => records,
            Err(e) => {
                self.left = 0;
                return Some(Err(e));
            }
        };
        if let Some(r) = range {
            if let Some(past) = records.iter().position(|rec| rec.time_nanos >= r.nanos().1) {
                records.truncate(past);
                self.segments = Vec::new().into_iter();
            }
            records.retain(|rec| r.holds(rec.time_nanos));
        }
        let records: Vec<pb::StreamRecord> = records.into_iter().map(record_to_pb).collect();
        let sent = u64::try_from(records.len()).unwrap_or(u64::MAX);
        self.left = self.left.saturating_sub(sent);
        self.next = match records.last() {
            Some(r) if self.left == 0 => r.offset + 1,
            _ => s.end_offset,
        };
        Some(Ok(pb::ConsoleChunk {
            records,
            missing: missing.into(),
            next_offset: self.next,
            ..Default::default()
        }))
    }
}

/// The chunks of `read` from `from`, at most `limit` records; zero is no limit.
pub(super) fn chunks(
    read: Offloaded,
    repos: BTreeMap<Uuid, Repo<S3Store>>,
    (from, limit): (u64, u64),
) -> impl futures_util::Stream<Item = Result<pb::ConsoleChunk, ConnectError>> + Send {
    let left = if limit == 0 { u64::MAX } else { limit };
    let range = read.stream_range;
    let state = Reading { segments: read.segments.into_iter(), repos, next: from, left, range };
    futures_util::stream::unfold(state, |mut state| async move {
        let item = state.chunk().await?;
        Some((item, state))
    })
}
