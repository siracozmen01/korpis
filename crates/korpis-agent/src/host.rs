// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the agent asks of the node beyond the runtime: the privileged helper's volume verbs
//! (§6 of docs/02-architecture.md), spoken in the helper protocol's own messages, and the
//! node-local content store, which the agent owns and drivers only reference by digest (§6 of
//! docs/04-runtimes.md). The helper is a separate root process and the content store sits in
//! front of the runtime's; adapters over both implement these, and so does [`InMemoryHost`].

mod memory;
pub mod node;
pub mod privileged;

pub use memory::InMemoryHost;

use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::future::Future;

/// Why the helper or the content store failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HostError(pub String);

/// What is asked of a volume's snapshot (§4 of docs/06-storage.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotOp {
    /// Take it, read-only; idempotent.
    Take,
    /// Make the volume what it holds; reached only from an approved, irreversible step, with
    /// the workload stopped.
    Rollback,
    /// Remove it; reached only from an approved, irreversible step.
    Delete,
}

/// The node's helper and content store.
pub trait Host: Send + Sync + 'static {
    /// Creates a volume, or finds the one already created; idempotent by volume.
    fn ensure_volume(
        &self,
        request: &helper::EnsureVolumeRequest,
    ) -> impl Future<Output = Result<helper::EnsureVolumeResponse, HostError>> + Send;

    /// Removes a volume and its data: reached only from an approved, irreversible step.
    fn delete_volume(&self, volume: &str) -> impl Future<Output = Result<(), HostError>> + Send;

    /// Does `op` to snapshot `snapshot` of volume `volume`.
    fn snapshot(
        &self,
        op: SnapshotOp,
        volume: &str,
        snapshot: &str,
    ) -> impl Future<Output = Result<(), HostError>> + Send;

    /// Fetches the content `reference` names onto the node, verified against its digest;
    /// idempotent, and shared by every workload that references it.
    fn fetch(&self, reference: &str) -> impl Future<Output = Result<(), HostError>> + Send;

    /// Fetches the artifact `reference` names, a recipe's, with its files; each file's path in
    /// the artifact and its digest (§2 of docs/09-recipes.md).
    fn fetch_files(
        &self,
        reference: &str,
    ) -> impl Future<Output = Result<BTreeMap<String, String>, HostError>> + Send;

    /// The size of content `digest`, a recipe's file or what a fetch downloaded, if the node
    /// holds it.
    fn content_size(
        &self,
        digest: &str,
    ) -> impl Future<Output = Result<Option<u64>, HostError>> + Send;

    /// The bytes of content `digest` the node holds, a recipe's file among them; more than
    /// `limit` of them is refused.
    fn read_content(
        &self,
        digest: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<u8>, HostError>> + Send;

    /// Fetches what `url` serves, kept only if it hashes to `digest`; idempotent by digest
    /// (§4 of docs/09-recipes.md).
    fn fetch_url(
        &self,
        url: &str,
        digest: &str,
    ) -> impl Future<Output = Result<(), HostError>> + Send;

    /// Records the id range `workload` holds on this node, as its assignment names it (§4.5 of
    /// docs/04-runtimes.md); a range that is not one workload's is refused.
    fn ensure_ids(
        &self,
        workload: &str,
        base: u32,
        count: u32,
    ) -> impl Future<Output = Result<(), HostError>> + Send;

    /// The storage classes the node offers, as the helper declares them.
    fn storage(&self) -> impl Future<Output = Result<Vec<pb::StorageClassInfo>, HostError>> + Send;

    /// Starts a backup or restore job, or finds the one started; whether it started now.
    fn start_job(
        &self,
        request: &helper::StartJobRequest,
    ) -> impl Future<Output = Result<bool, HostError>> + Send;

    /// Where a job is.
    fn job_status(
        &self,
        job: &str,
    ) -> impl Future<Output = Result<helper::JobStatusResponse, HostError>> + Send;

    /// Removes what is kept of a job that ended, discarding a restore it staged and never
    /// committed.
    fn forget_job(&self, job: &str) -> impl Future<Output = Result<(), HostError>> + Send;

    /// Makes the volume what a replacing restore that finished whole staged; idempotent.
    fn commit_job(&self, job: &str) -> impl Future<Output = Result<(), HostError>> + Send;

    /// The jobs kept, and where each is.
    fn list_jobs(&self) -> impl Future<Output = Result<Vec<helper::JobSummary>, HostError>> + Send;

    /// Makes a workload's network what `request` says (§4.1 of docs/07-networking.md).
    fn ensure_network(
        &self,
        request: &helper::EnsureNetworkRequest,
    ) -> impl Future<Output = Result<helper::EnsureNetworkResponse, HostError>> + Send;

    /// Removes a workload's network; whether there was one.
    fn release_network(
        &self,
        workload: &str,
    ) -> impl Future<Output = Result<bool, HostError>> + Send;

    /// The sockets of a workload's resolver, opened inside its network namespace: UDP, then TCP
    /// listening (§5.1 of docs/07-networking.md).
    fn open_resolver(
        &self,
        workload: &str,
    ) -> impl Future<Output = Result<Vec<std::os::fd::OwnedFd>, HostError>> + Send;

    /// A socket for probing a workload's health (§5 of docs/04-runtimes.md), a datagram socket
    /// if `udp` and a stream otherwise, opened inside its network namespace, neither bound nor
    /// connected.
    fn open_probe(
        &self,
        workload: &str,
        udp: bool,
    ) -> impl Future<Output = Result<std::os::fd::OwnedFd, HostError>> + Send;

    /// Ports 80 and 443 of each of `addresses`, opened listening for the ingress proxy: every
    /// port 80, then every 443 (§3.2 of docs/07-networking.md).
    fn open_ingress(
        &self,
        addresses: &[String],
    ) -> impl Future<Output = Result<Vec<std::os::fd::OwnedFd>, HostError>> + Send;

    /// Starts the ingress proxy, connecting to the supervisor's socket at `control`; one started
    /// before is stopped first. What confines it, as the helper could.
    fn start_ingress(
        &self,
        control: &std::path::Path,
    ) -> impl Future<Output = Result<helper::StartIngressResponse, HostError>> + Send;

    /// Stops the ingress proxy; whether it ran.
    fn stop_ingress(&self) -> impl Future<Output = Result<bool, HostError>> + Send;

    /// A file session on a workload's volumes (§3.2 of docs/06-storage.md): the socket of the
    /// worker serving it, on which each `FileRequest` is written and each `FileResponse` read as
    /// four bytes of little-endian length and the message.
    fn open_files(
        &self,
        request: &helper::OpenFilesRequest,
    ) -> impl Future<Output = Result<std::os::fd::OwnedFd, HostError>> + Send;

    /// Whether the node can give workloads networks, and what stands in the way.
    fn describe_network(
        &self,
    ) -> impl Future<Output = Result<helper::DescribeNetworkResponse, HostError>> + Send;

    /// Makes the node's uplinks shape what its workloads send, exactly those `request` names
    /// (§7 of docs/07-networking.md).
    fn ensure_uplinks(
        &self,
        request: &helper::EnsureUplinksRequest,
    ) -> impl Future<Output = Result<helper::EnsureUplinksResponse, HostError>> + Send;

    /// What the host's table has counted of every workload's link: each endpoint whose
    /// accounting is on, and its egress (§7 of docs/07-networking.md).
    fn read_counters(
        &self,
    ) -> impl Future<Output = Result<helper::ReadCountersResponse, HostError>> + Send;

    /// What the filesystem accounts for `volume`, read rather than computed (§3 of
    /// docs/06-storage.md): what the meter samples as its stored bytes (§2 of
    /// docs/15-observability.md).
    fn volume_usage(
        &self,
        volume: &str,
    ) -> impl Future<Output = Result<helper::VolumeUsageResponse, HostError>> + Send;

    /// Keeps the workloads' cpusets to the CPUs `request` says the node's dedicated cores hold,
    /// under the drivers' cgroup parents it names (§5.2 of docs/05-scheduling.md).
    fn hold_cores(
        &self,
        request: &helper::HoldCoresRequest,
    ) -> impl Future<Output = Result<helper::HoldCoresResponse, HostError>> + Send;

    /// Keeps the node's side of the mesh to `request` (§9.1 of docs/07-networking.md): its
    /// interface, its peers, and the routes to them; its public key and the peers' handshakes.
    fn keep_overlay(
        &self,
        request: &helper::KeepOverlayRequest,
    ) -> impl Future<Output = Result<helper::KeepOverlayResponse, HostError>> + Send;
}
