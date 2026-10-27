// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes as the control plane knows them (§3.6 of docs/01-model.md, §7 of docs/06-storage.md):
//! durable state with a lifecycle of its own. A node creates a volume when a workload needs it
//! and reports every one it ensures or deletes; the control plane keeps the object from those
//! reports, so a volume stays listed after the workload it was made for is gone, and a volume
//! no workload holds is an orphan to be listed, never collected.

use crate::node::StorageClass;
use std::time::SystemTime;
use uuid::Uuid;

/// A block-device volume's image, in its volume's directory on the node (§3.1 of
/// docs/06-storage.md): what the helper makes and the driver gives its guest.
pub const BLOCK_IMAGE: &str = "block.ext4";

/// The extended attribute on a volume's directory that says it holds a block device, and its
/// value: in the `trusted` namespace, which no workload can set.
pub const BLOCK_MARK: (&str, &[u8]) = ("trusted.korpis.volume", b"block_device");

/// Why a volume exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumeKind {
    /// The workload declared it.
    Declared,
    /// It holds the workload's console stream (§4 of docs/14-streams.md).
    Stream,
}

impl VolumeKind {
    /// Its stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Stream => "stream",
        }
    }

    /// The kind a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Declared, Self::Stream].into_iter().find(|k| k.as_str() == s)
    }
}

/// A volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// Immutable.
    pub id: Uuid,
    /// The project it belongs to.
    pub project: Uuid,
    /// The workload it was created for.
    pub created_by: Uuid,
    /// The workload it is attached to, while one holds it.
    pub attached_to: Option<Uuid>,
    /// Its name in the workload that created it.
    pub name: String,
    /// Why it exists.
    pub kind: VolumeKind,
    /// The node whose helper holds it, for a volume on one node's disk; `None` for one in a
    /// shared class, which every node offering its store reaches (§2.1 of docs/06-storage.md).
    pub node: Option<Uuid>,
    /// The storage a shared class's volume is in; `None` for one on a node's disk.
    pub store: Option<String>,
    /// The node that last reported it: its own node, or for a shared one the node a plan on it
    /// runs on when no workload holds it.
    pub reported_by: Uuid,
    /// The privileged helper's identifier for it on its node, or in its store.
    pub helper_id: String,
    /// Its storage class.
    pub storage_class: String,
    /// The size asked for.
    pub size_bytes: u64,
    /// Whether that size is enforced in the write path.
    pub quota_enforced: bool,
    /// When a node first reported it.
    pub created: SystemTime,
    /// When the workload it was attached to let go of it.
    pub detached_at: Option<SystemTime>,
    /// When it was deleted, data and all.
    pub deleted_at: Option<SystemTime>,
    /// Its live snapshots, oldest first.
    pub snapshots: Vec<Uuid>,
}

/// Where a volume's data is, which decides the nodes that reach it (§2.1 of
/// docs/06-storage.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Home {
    /// On one node's disk.
    Node(Uuid),
    /// In a shared store, which every node offering a class of it reaches.
    Store(String),
}

impl Home {
    /// Whether a node `node`, offering `classes`, reaches it.
    #[must_use]
    pub fn reached_by(&self, node: Uuid, classes: &[StorageClass]) -> bool {
        match self {
            Self::Node(n) => *n == node,
            Self::Store(s) => classes.iter().any(|c| c.store.as_deref() == Some(s.as_str())),
        }
    }
}

impl std::fmt::Display for Home {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Node(n) => write!(f, "on node {n}"),
            Self::Store(s) => write!(f, "in store {s}"),
        }
    }
}

impl Volume {
    /// Where its data is.
    #[must_use]
    pub fn home(&self) -> Home {
        match (&self.store, self.node) {
            (Some(s), _) => Home::Store(s.clone()),
            (None, Some(n)) => Home::Node(n),
            (None, None) => Home::Node(self.reported_by),
        }
    }

    /// The node a plan on it runs on when no workload holds it: its own, or for a shared one the
    /// node that last reported it, which reaches its store.
    #[must_use]
    pub fn keeper(&self) -> Uuid {
        self.node.unwrap_or(self.reported_by)
    }

    /// The size a client may be shown: the one the write path enforces, and none where nothing
    /// does (Rule K-3).
    #[must_use]
    pub const fn shown_size(&self) -> Option<u64> {
        if self.quota_enforced { Some(self.size_bytes) } else { None }
    }
}

/// What happened to a volume on its node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeEvent {
    /// It was created, or found already there.
    Ensured,
    /// It was deleted.
    Deleted,
}

/// What a node reports of a volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedVolume {
    /// The workload it is for.
    pub workload: Uuid,
    /// Its name there.
    pub name: String,
    /// The helper's identifier for it.
    pub helper_id: String,
    /// Its class.
    pub storage_class: String,
    /// The size asked for.
    pub size_bytes: u64,
    /// Whether that size is enforced.
    pub quota_enforced: bool,
    /// What happened.
    pub event: VolumeEvent,
    /// Why it exists.
    pub kind: VolumeKind,
}

/// A snapshot of a volume (§4 of docs/06-storage.md): a point the volume can be rolled back to,
/// on the volume's node and filesystem, and so no protection against losing either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Immutable; the helper knows it by its wire form.
    pub id: Uuid,
    /// The volume it is of.
    pub volume: Uuid,
    /// Its name, unique among the volume's live snapshots.
    pub name: String,
    /// When its node took it.
    pub created: SystemTime,
    /// When it was deleted.
    pub deleted_at: Option<SystemTime>,
}

/// What happened to a snapshot on its node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotEvent {
    /// It was taken, or found already taken.
    Taken,
    /// Its volume was rolled back to it.
    RolledBack,
    /// It was deleted.
    Deleted,
}

impl SnapshotEvent {
    /// The action its effect records.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Taken => "volume.snapshot.taken",
            Self::RolledBack => "volume.snapshot.rolled_back",
            Self::Deleted => "volume.snapshot.deleted",
        }
    }
}

/// What a node reports of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedSnapshot {
    /// The workload whose plan it was.
    pub workload: Uuid,
    /// The helper's identifier of the volume.
    pub volume: String,
    /// The snapshot.
    pub snapshot: Uuid,
    /// Its name, for the record.
    pub name: String,
    /// What happened.
    pub event: SnapshotEvent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_shown_only_where_it_is_enforced() {
        let mut v = Volume {
            id: Uuid::now_v7(),
            project: Uuid::now_v7(),
            created_by: Uuid::now_v7(),
            attached_to: None,
            name: "data".into(),
            kind: VolumeKind::Declared,
            node: Some(Uuid::now_v7()),
            store: None,
            reported_by: Uuid::now_v7(),
            helper_id: "wl.data".into(),
            storage_class: "local".into(),
            size_bytes: 1 << 30,
            quota_enforced: false,
            created: SystemTime::UNIX_EPOCH,
            detached_at: None,
            deleted_at: None,
            snapshots: Vec::new(),
        };
        assert_eq!(v.shown_size(), None);
        v.quota_enforced = true;
        assert_eq!(v.shown_size(), Some(1 << 30));
        assert_eq!(VolumeKind::parse("stream"), Some(VolumeKind::Stream));
    }
}
