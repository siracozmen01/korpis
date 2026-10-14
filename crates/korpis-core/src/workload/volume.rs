// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes as the control plane knows them (§3.6 of docs/01-model.md, §7 of docs/06-storage.md):
//! durable state with a lifecycle of its own. A node creates a volume when a workload needs it
//! and reports every one it ensures or deletes; the control plane keeps the object from those
//! reports, so a volume stays listed after the workload it was made for is gone, and a volume
//! no workload holds is an orphan to be listed, never collected.

use std::time::SystemTime;
use uuid::Uuid;

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
    /// The node that reported it, whose helper holds it.
    pub node: Uuid,
    /// The privileged helper's identifier for it on its node.
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
}

impl Volume {
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
            node: Uuid::now_v7(),
            helper_id: "wl.data".into(),
            storage_class: "local".into(),
            size_bytes: 1 << 30,
            quota_enforced: false,
            created: SystemTime::UNIX_EPOCH,
            detached_at: None,
            deleted_at: None,
        };
        assert_eq!(v.shown_size(), None);
        v.quota_enforced = true;
        assert_eq!(v.shown_size(), Some(1 << 30));
        assert_eq!(VolumeKind::parse("stream"), Some(VolumeKind::Stream));
    }
}
