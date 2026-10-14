// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a backup in the control plane (§5.4 of docs/06-storage.md, Finding 13 of
//! docs/23-walkthroughs.md): browsing its tree under `backup.browse`, which shows every path,
//! name, and size, and reading a file under `backup.read`, which shows its bytes. Neither implies
//! the other, both are recorded as sensitive reads before anything is read (§4 of
//! docs/15-observability.md), and both are done with read access to the workload's prefix and
//! the workload's key.

use super::job::JobGrant;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, EffectWrite as _, RecipeSource, Store, Tx as _};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::backup::BackupStatus;
use korpis_core::extension::AccessMode;
use korpis_core::object::{Kind, ObjectRef};
use std::time::Duration;
use uuid::Uuid;

/// How long the control plane's own read access lasts: minutes for one listing, and for one
/// file as long as a job's, since a large file over a slow link takes as long to read as to
/// restore (Finding 36 of docs/23-walkthroughs.md).
const BROWSE_ACCESS: Duration = Duration::from_secs(15 * 60);
const READ_ACCESS: Duration = Duration::from_secs(12 * 3600);

/// What a backup is read for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFor {
    /// Its tree: `backup.browse`.
    Browse,
    /// A file's bytes: `backup.read`.
    Read,
}

impl ReadFor {
    const fn lasting(self) -> Duration {
        match self {
            Self::Browse => BROWSE_ACCESS,
            Self::Read => READ_ACCESS,
        }
    }

    const fn action(self) -> &'static str {
        match self {
            Self::Browse => "backup.browse",
            Self::Read => "backup.read",
        }
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The key and access to read `path` of backup `backup` for `what`, recorded as read.
    pub async fn read_backup(
        &self,
        caller: &Caller,
        (backup, path): (Uuid, &str),
        what: ReadFor,
    ) -> Result<JobGrant, AppError> {
        let b = self.store.backup(backup).await?.filter(|b| b.deleted_at.is_none());
        let b = b.ok_or(AppError::NotFound { kind: "backup" })?;
        let object = ObjectRef::new(Kind::Workload, b.workload);
        let permit = self.authorize(caller, what.action(), object).await?;
        if b.status != BackupStatus::Succeeded {
            return Err(AppError::conflict("backup.not_whole", "only a whole backup is read"));
        }
        let repo = self.store.repository(b.repository).await?;
        let repo = repo.ok_or(AppError::NotFound { kind: "repository" })?;
        let mut e = Effect::new(caller.principal.clone(), what.action(), Outcome::Succeeded)
            .under(permit.grant)
            .after(details([("backup", b.id.to_string().into()), ("path", path.into())]));
        e.workload = Some(b.workload);
        let mut tx = self.store.begin().await?;
        tx.append_effect(&e).await?;
        tx.commit().await?;
        self.record_access((repo.id, b.workload), Some(b.id), AccessMode::Read, what.lasting())
            .await?;
        self.control_plane_access(&repo, b.workload, what.lasting()).await
    }
}
