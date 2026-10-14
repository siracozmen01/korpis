// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node offloading a workload's console stream is given (§4 and question 1 of
//! docs/14-streams.md): the workload's data key in the repository that takes its streams, and an
//! hour of write access to the workload's stream prefix there, beside its backups and never
//! beneath them. Asked by a node that held the workload's lease, retired or not, since a stream's
//! last segment seals as its container goes. Where no repository takes the workload's streams,
//! nothing is given, and the stream says how much of it is only on its node.

use super::job::{JobGrant, JobRefusal};
use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{Area, Clock, RecipeSource, Store};
use korpis_core::extension::{AccessMode, CallOutcome, ProviderType};
use std::time::Duration;
use uuid::Uuid;

/// How long a node may offload with one grant before it asks again.
const OFFLOAD_ACCESS: Duration = Duration::from_secs(3600);

type Judged<T> = Result<Result<T, JobRefusal>, AppError>;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The key and access `node` offloads `workload`'s console stream with.
    pub async fn grant_offload_access(&self, node: Uuid, workload: Uuid) -> Judged<JobGrant> {
        let Some(w) = self.store.workload(workload).await? else {
            return Ok(Err(JobRefusal::NotAssigned));
        };
        if !self.store.ever_leased(workload, node).await? {
            return Ok(Err(JobRefusal::NotAssigned));
        }
        let Some(repo) = self.store.offload_repository(w.organization, w.project).await? else {
            return Ok(Err(JobRefusal::NoTarget));
        };
        let data_key = self.data_key(&repo, w.id, true).await?;
        let prefix = ((repo.id, w.id), Area::Streams);
        self.record_access(prefix, None, AccessMode::Write, OFFLOAD_ACCESS).await?;
        let target = CallTarget {
            organization: repo.organization,
            kind: ProviderType::BackupTarget,
            name: repo.provider.clone(),
        };
        let place = repo.stream_prefix(w.id);
        let called =
            self.grant_backup_access(&target, &place, AccessMode::Write, OFFLOAD_ACCESS).await?;
        match called.outcome {
            CallOutcome::Access(access) => Ok(Ok(JobGrant { data_key, access })),
            CallOutcome::Failed(f) => Ok(Err(JobRefusal::Store(f.reason))),
            _ => Ok(Err(JobRefusal::Store("provider.malformed".into()))),
        }
    }
}
