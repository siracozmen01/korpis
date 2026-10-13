// SPDX-License-Identifier: AGPL-3.0-or-later

//! An installation (§4 of docs/16-extensions.md): an extension's artifact installed into an
//! organization or one of its projects, running as one workload, and the subject its grants are
//! issued to.

use crate::principal::Principal;
use std::time::SystemTime;
use uuid::Uuid;

/// An installed extension, or one that was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    /// Its identifier; its subject is `extension:<id>`.
    pub id: Uuid,
    /// The organization it is installed into.
    pub organization: Uuid,
    /// The project, when it is installed into one rather than the whole organization.
    pub project: Option<Uuid>,
    /// Its document's name.
    pub name: String,
    /// Its document's version.
    pub version: String,
    /// The contract it speaks.
    pub api_version: String,
    /// The workload it runs as.
    pub workload: Uuid,
    /// Its artifact's repository.
    pub artifact_name: String,
    /// Its artifact's digest, the only version its workload may run.
    pub artifact_digest: String,
    /// Who installed it.
    pub installed_by: Principal,
    /// When.
    pub installed_at: SystemTime,
    /// When it was uninstalled; its grants were revoked then.
    pub uninstalled_at: Option<SystemTime>,
}

impl Installation {
    /// The principal it acts as.
    #[must_use]
    pub const fn principal(&self) -> Principal {
        Principal::Extension(self.id)
    }

    /// Whether it is installed now.
    #[must_use]
    pub const fn is_installed(&self) -> bool {
        self.uninstalled_at.is_none()
    }
}
