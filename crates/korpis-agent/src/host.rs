// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the agent asks of the node beyond the runtime: the privileged helper's volume verbs
//! (§6 of docs/02-architecture.md), spoken in the helper protocol's own messages, and the
//! node-local content store, which the agent owns and drivers only reference by digest (§6 of
//! docs/04-runtimes.md). The helper is a separate root process and the content store sits in
//! front of the runtime's; adapters over both implement these, and so does [`InMemoryHost`].

pub mod node;
pub mod privileged;

use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::collections::{BTreeMap, BTreeSet};
use std::future::{Future, ready};
use std::sync::{Arc, Mutex};

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
}

#[derive(Debug, Default)]
struct Kept {
    volumes: BTreeMap<String, String>,
    content: BTreeSet<String>,
    ids: BTreeMap<String, (u32, u32)>,
    snapshots: BTreeSet<(String, String)>,
}

/// A helper and content store that keep their records in memory; clones share them.
#[derive(Debug, Clone, Default)]
pub struct InMemoryHost {
    kept: Arc<Mutex<Kept>>,
}

impl InMemoryHost {
    /// Nothing kept.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with<T>(&self, f: impl FnOnce(&mut Kept) -> T) -> Result<T, HostError> {
        let mut kept = self.kept.lock().map_err(|_| HostError("poisoned".into()))?;
        Ok(f(&mut kept))
    }

    /// The volumes, by identifier, with their paths.
    #[must_use]
    pub fn volumes(&self) -> BTreeMap<String, String> {
        self.with(|k| k.volumes.clone()).unwrap_or_default()
    }

    /// The content fetched.
    #[must_use]
    pub fn content(&self) -> BTreeSet<String> {
        self.with(|k| k.content.clone()).unwrap_or_default()
    }

    /// The snapshots, by volume and snapshot.
    #[must_use]
    pub fn snapshots(&self) -> BTreeSet<(String, String)> {
        self.with(|k| k.snapshots.clone()).unwrap_or_default()
    }

    /// The id ranges recorded, by workload: base and count.
    #[must_use]
    pub fn ids(&self) -> BTreeMap<String, (u32, u32)> {
        self.with(|k| k.ids.clone()).unwrap_or_default()
    }
}

impl Host for InMemoryHost {
    fn ensure_volume(
        &self,
        request: &helper::EnsureVolumeRequest,
    ) -> impl Future<Output = Result<helper::EnsureVolumeResponse, HostError>> + Send {
        let volume = request.volume.clone();
        ready(self.with(|k| {
            let created = !k.volumes.contains_key(&volume);
            let path = k.volumes.entry(volume.clone()).or_insert(format!("/memory/{volume}"));
            helper::EnsureVolumeResponse {
                host_path: path.clone(),
                created,
                storage_class: request.storage_class.clone(),
                ..Default::default()
            }
        }))
    }

    fn delete_volume(&self, volume: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.volumes.remove(volume);
        }))
    }

    fn snapshot(
        &self,
        op: SnapshotOp,
        volume: &str,
        snapshot: &str,
    ) -> impl Future<Output = Result<(), HostError>> + Send {
        let key = (volume.to_owned(), snapshot.to_owned());
        ready(
            self.with(|k| match op {
                SnapshotOp::Take if k.volumes.contains_key(volume) => {
                    k.snapshots.insert(key);
                    Ok(())
                }
                SnapshotOp::Rollback if k.snapshots.contains(&key) => Ok(()),
                SnapshotOp::Delete => {
                    k.snapshots.remove(&key);
                    Ok(())
                }
                _ => Err(HostError(format!("no volume {volume} with snapshot {snapshot}"))),
            })
            .and_then(|r| r),
        )
    }

    fn fetch(&self, reference: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.content.insert(reference.to_owned());
        }))
    }

    fn ensure_ids(
        &self,
        workload: &str,
        base: u32,
        count: u32,
    ) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.ids.insert(workload.to_owned(), (base, count));
        }))
    }

    fn storage(&self) -> impl Future<Output = Result<Vec<pb::StorageClassInfo>, HostError>> + Send {
        ready(Ok(Vec::new()))
    }
}
