// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node's backup or restore job is given, or why it is given nothing (§5.5 of
//! docs/06-storage.md).

use crate::ports::DataKey;
use korpis_core::extension::ObjectAccess;

/// One job's key and access.
#[derive(Debug)]
pub struct JobGrant {
    /// The workload's data key, opened for this job.
    pub data_key: DataKey,
    /// Access to the workload's prefix, until it expires.
    pub access: ObjectAccess,
}

/// Why a job is given nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobRefusal {
    /// The plan is not open, or the node does not hold the volume it operates on.
    NotAssigned,
    /// The step is not a backup or restore.
    NotAJob,
    /// The backup already ended.
    Ended,
    /// The backup target did not grant access, for the provider's reason.
    Store(String),
}

impl JobRefusal {
    /// The stable reason the node is told.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::NotAssigned => "job.not_assigned".into(),
            Self::NotAJob => "job.not_a_job".into(),
            Self::Ended => "job.ended".into(),
            Self::Store(reason) => reason.clone(),
        }
    }
}
