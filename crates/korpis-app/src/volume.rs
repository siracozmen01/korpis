// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading volumes (§7 of docs/06-storage.md): what nodes reported of them, through the project
//! they belong to, so a volume whose workload is gone is still listed, and an orphan is seen.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::{Page, Paged};
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::volume::{Snapshot, Volume};
use uuid::Uuid;

/// Which volumes a listing names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListVolumes {
    /// The project.
    pub project: Uuid,
    /// Only those created for or attached to this workload.
    pub workload: Option<Uuid>,
    /// Deleted ones too.
    pub include_deleted: bool,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// One volume the caller may read, deleted or not.
    pub async fn get_volume(&self, caller: &Caller, id: Uuid) -> Result<Volume, AppError> {
        self.authorize(caller, "volume.read", ObjectRef::new(Kind::Volume, id)).await?;
        self.store.volume(id).await?.ok_or(AppError::NotFound { kind: "volume" })
    }

    /// A volume's snapshots, oldest first, read under `volume.read` on it.
    pub async fn list_snapshots(
        &self,
        caller: &Caller,
        volume: Uuid,
        include_deleted: bool,
    ) -> Result<Vec<Snapshot>, AppError> {
        self.authorize(caller, "volume.read", ObjectRef::new(Kind::Volume, volume)).await?;
        Ok(self.store.snapshots(volume, include_deleted).await?)
    }

    /// The volumes of a project, read under `volume.read` on it.
    pub async fn list_volumes(
        &self,
        caller: &Caller,
        q: ListVolumes,
        page: Page,
    ) -> Result<Paged<Volume>, AppError> {
        self.authorize(caller, "volume.read", ObjectRef::new(Kind::Project, q.project)).await?;
        let items = self.store.volumes(q.project, q.workload, q.include_deleted, page).await?;
        let next = page.next(&items, |v| v.id);
        Ok(Paged { items, next })
    }
}
