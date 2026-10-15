// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node offloading a workload's console stream is given (§4 and question 1 of
//! docs/14-streams.md): the workload's data key in the repository that takes its streams, and an
//! hour of write access to the workload's stream prefix there, beside its backups and never
//! beneath them. Asked by a node that held the workload's lease, retired or not, since a stream's
//! last segment seals as its container goes. Where no repository takes the workload's streams,
//! nothing is given, and the stream says how much of it is only on its node.
//!
//! And what the control plane reads offloaded segments back with, for a caller who may read the
//! console, once their node is gone or the workload retired: the workload's key and minutes of
//! read access to its stream prefix in each repository holding them, the read recorded first.

use super::job::{JobGrant, JobRefusal};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{Area, Clock, EffectWrite as _, RecipeSource, Store, Tx as _};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef, StreamRange};
use korpis_core::extension::{AccessMode, CallOutcome, ProviderType};
use korpis_core::workload::stream::{Segment, StreamKind};
use std::collections::BTreeMap;
use std::time::Duration;
use uuid::Uuid;

/// How long a node may offload with one grant before it asks again.
const OFFLOAD_ACCESS: Duration = Duration::from_secs(3600);

/// How long the control plane reads offloaded segments with one grant: the shortest a target
/// grants, and minutes of segments at most.
const READ_ACCESS: Duration = Duration::from_secs(15 * 60);

/// The offloaded segments of a stream from an offset, and what reads them.
#[derive(Debug)]
pub struct Offloaded {
    /// The segments holding offsets from the one asked for, in offset order, each with the
    /// repository holding it.
    pub segments: Vec<(Segment, Uuid)>,
    /// The key and read access for each repository holding one.
    pub grants: BTreeMap<Uuid, JobGrant>,
    /// The only records the caller may read, when its grant is limited to a range of the stream
    /// (§8 of docs/14-streams.md): whatever sends the records sends those alone.
    pub stream_range: Option<StreamRange>,
}

type Judged<T> = Result<Result<T, JobRefusal>, AppError>;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The offloaded segments of `workload`'s stream of `kind` holding offsets from `from`, and
    /// what reads them, for `caller`, who must hold `workload.console.read`. The read is recorded
    /// before any access is asked for. A segment whose repository is deleted is not read.
    pub async fn read_offloaded(
        &self,
        caller: &Caller,
        (workload, kind): (Uuid, StreamKind),
        from: u64,
    ) -> Result<Offloaded, AppError> {
        let on = ObjectRef::new(Kind::Workload, workload);
        let permit = self.authorize(caller, "workload.console.read", on).await?;
        let stream = self.store.stream(workload, kind).await?;
        let stream = stream.ok_or(AppError::NotFound { kind: "stream" })?;
        let wanted: Vec<_> = stream.segments.into_iter().filter(|s| s.end_offset > from).collect();
        let mut held = BTreeMap::new();
        for id in wanted.iter().filter_map(|s| s.repository) {
            if let std::collections::btree_map::Entry::Vacant(v) = held.entry(id) {
                let repo = self.store.repository(id).await?.filter(|r| r.deleted_at.is_none());
                v.insert(repo);
            }
        }
        let segments: Vec<_> = wanted
            .into_iter()
            .filter_map(|s| {
                s.repository
                    .filter(|id| held.get(id).is_some_and(Option::is_some))
                    .map(|id| (s, id))
            })
            .collect();
        let mut grants = BTreeMap::new();
        let stream_range = permit.stream_range;
        if segments.is_empty() {
            return Ok(Offloaded { segments, grants, stream_range });
        }
        let mut e =
            Effect::new(caller.principal.clone(), "workload.console.read", Outcome::Succeeded)
                .under(permit.grant)
                .after(details([
                    ("stream", kind.as_str().into()),
                    ("from_offset", from.into()),
                    ("offloaded", true.into()),
                    ("ranged", stream_range.is_some().into()),
                ]));
        e.workload = Some(workload);
        let mut tx = self.store.begin().await?;
        tx.append_effect(&e).await?;
        tx.commit().await?;
        for repo in held.into_values().flatten() {
            let prefix = ((repo.id, workload), Area::Streams);
            self.record_access(prefix, (None, None), AccessMode::Read, READ_ACCESS).await?;
            let grant =
                self.control_plane_access((&repo, Area::Streams), workload, READ_ACCESS).await?;
            grants.insert(repo.id, grant);
        }
        Ok(Offloaded { segments, grants, stream_range })
    }

    /// The key and access `node` offloads `workload`'s console stream with.
    /// `recipient` is the one-time public key the node made, as for a job.
    pub async fn grant_offload_access(
        &self,
        node: Uuid,
        workload: Uuid,
        recipient: &[u8],
    ) -> Judged<JobGrant> {
        let Some(w) = self.store.workload(workload).await? else {
            return Ok(Err(JobRefusal::NotAssigned));
        };
        if !self.store.ever_leased(workload, node).await? {
            return Ok(Err(JobRefusal::NotAssigned));
        }
        let Some(repo) = self.store.offload_repository(w.organization, w.project).await? else {
            return Ok(Err(JobRefusal::NoTarget));
        };
        let data_key = self.job_key((&repo, w.id), true, recipient).await?;
        let prefix = ((repo.id, w.id), Area::Streams);
        self.record_access(prefix, (None, Some(node)), AccessMode::Write, OFFLOAD_ACCESS).await?;
        let target = CallTarget {
            organization: repo.organization,
            kind: ProviderType::BackupTarget,
            name: repo.provider.clone(),
        };
        let place = repo.stream_prefix(w.id);
        let called =
            self.grant_backup_access(&target, &place, AccessMode::Write, OFFLOAD_ACCESS).await?;
        match called.outcome {
            CallOutcome::Access(access) => Ok(Ok(JobGrant {
                repository: repo.id,
                workload: w.id,
                data_key,
                access,
                content: None,
            })),
            CallOutcome::Failed(f) => Ok(Err(JobRefusal::Store(f.reason))),
            _ => Ok(Err(JobRefusal::Store("provider.malformed".into()))),
        }
    }
}
