// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's real [`Host`]: the privileged helper over its socket for volumes, id ranges, and
//! storage, and the agent's own content store for what workloads run from (§6 of
//! docs/04-runtimes.md), which the agent fills and drivers only read.

use super::privileged::HelperClient;
use super::{Host, HostError, SnapshotOp};
use crate::content::ContentStore;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::future::Future;
use std::sync::Arc;

/// The helper and the content store; clones share them.
#[derive(Debug, Clone)]
pub struct NodeHost {
    helper: HelperClient,
    content: Arc<ContentStore>,
}

impl NodeHost {
    /// The helper reached by `helper`, and the content store `content`.
    #[must_use]
    pub fn new(helper: HelperClient, content: ContentStore) -> Self {
        Self { helper, content: Arc::new(content) }
    }
}

impl Host for NodeHost {
    fn ensure_volume(
        &self,
        request: &helper::EnsureVolumeRequest,
    ) -> impl Future<Output = Result<helper::EnsureVolumeResponse, HostError>> + Send {
        self.helper.ensure_volume(request)
    }

    async fn delete_volume(&self, volume: &str) -> Result<(), HostError> {
        self.helper.delete_volume(volume).await.map(|_| ())
    }

    async fn fetch(&self, reference: &str) -> Result<(), HostError> {
        let content = Arc::clone(&self.content);
        content
            .fetch(reference)
            .await
            .map(|digest| tracing::debug!(%reference, %digest, "content present"))
            .map_err(|e| HostError(format!("fetching {reference}: {e}")))
    }

    fn ensure_ids(
        &self,
        workload: &str,
        base: u32,
        count: u32,
    ) -> impl Future<Output = Result<(), HostError>> + Send {
        self.helper.ensure_id_range(workload, base, count)
    }

    fn storage(&self) -> impl Future<Output = Result<Vec<pb::StorageClassInfo>, HostError>> + Send {
        self.helper.describe_storage()
    }

    fn snapshot(
        &self,
        op: SnapshotOp,
        volume: &str,
        snapshot: &str,
    ) -> impl Future<Output = Result<(), HostError>> + Send {
        self.helper.snapshot(op, volume, snapshot)
    }

    fn start_job(
        &self,
        request: &helper::StartJobRequest,
    ) -> impl Future<Output = Result<bool, HostError>> + Send {
        self.helper.start_job(request)
    }

    fn job_status(
        &self,
        job: &str,
    ) -> impl Future<Output = Result<helper::JobStatusResponse, HostError>> + Send {
        self.helper.job_status(job)
    }

    fn forget_job(&self, job: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        self.helper.forget_job(job)
    }

    fn commit_job(&self, job: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        self.helper.commit_job(job)
    }

    fn list_jobs(&self) -> impl Future<Output = Result<Vec<helper::JobSummary>, HostError>> + Send {
        self.helper.list_jobs()
    }
}
