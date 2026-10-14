// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes created from a backup (§5.4 of docs/06-storage.md): a declared volume may name a
//! backup it is created holding, which its plan restores into it before anything runs on it.
//! That puts the backup's contents in the hands of the declaring workload's holders, so declaring
//! it takes `backup.read` on the backup's workload, and only a whole backup is named. It is
//! checked for the volumes the declaration creates, the ones its approved body does not already
//! have: a volume made long ago from a backup since pruned is not declared again.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::backup::BackupStatus;
use korpis_core::workload::IntentBody;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Checks each backup `body` creates a volume from, beyond those of `approved`.
    pub(super) async fn check_seeds(
        &self,
        caller: &Caller,
        approved: Option<&IntentBody>,
        body: &IntentBody,
    ) -> Result<(), AppError> {
        let created =
            |name: &str| approved.is_none_or(|a| a.volumes.iter().all(|v| v.name != name));
        for (i, v) in body.volumes.iter().enumerate() {
            let Some(id) = v.from_backup.filter(|_| created(&v.name)) else { continue };
            let field = format!("volumes[{i}].from_backup");
            let b = self.store.backup(id).await?.filter(|b| b.deleted_at.is_none());
            let b = b.ok_or_else(|| AppError::invalid(&field, "no such backup"))?;
            self.authorize(caller, "backup.read", ObjectRef::new(Kind::Workload, b.workload))
                .await?;
            if b.status != BackupStatus::Succeeded {
                return Err(AppError::invalid(field, "only a whole backup restores"));
            }
        }
        Ok(())
    }
}
