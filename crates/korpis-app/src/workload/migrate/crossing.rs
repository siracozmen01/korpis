// SPDX-License-Identifier: AGPL-3.0-or-later

//! How each of a migrating workload's volumes crosses (§8.1 of docs/05-scheduling.md), decided by
//! the classes both nodes declared at their last `Hello`: by increments where both send them from
//! the same backend, by its tree where both helpers declare `tree_sync`, not at all for a shared
//! or block class, and otherwise not: the migration is refused, naming the volume and the node
//! that cannot. Every volume attached to the workload crosses, its console's among them.

use crate::App;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::node::Node;
use korpis_core::node::driver::class_named;
use korpis_core::workload::Workload;
use korpis_core::workload::migration::{MigrationVolume, crossing};
use korpis_core::workload::plan::compute::Crossed;

/// `v` as a migration's plan names it.
pub(super) fn crossed(v: &MigrationVolume) -> Crossed {
    Crossed { name: v.name.clone(), size_bytes: v.size_bytes, crossing: v.crossing }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Each volume attached to `w`, as it crosses from `source` to `destination`.
    pub(in crate::workload) async fn crossings(
        &self,
        w: &Workload,
        (source, destination): (&Node, &Node),
    ) -> Result<Vec<MigrationVolume>, AppError> {
        let page = Page::new(None, Page::MAX);
        let held = self.store.volumes(w.project, Some(w.id), false, page).await?;
        let mut out = Vec::new();
        for v in held.iter().filter(|v| v.attached_to == Some(w.id) && v.deleted_at.is_none()) {
            let from = class_named(&source.storage_classes, &v.storage_class).ok_or_else(|| {
                AppError::Internal(format!("volume {} is in no class of its node", v.id))
            })?;
            let to = class_named(&destination.storage_classes, &v.storage_class);
            let how = crossing(from, to).map_err(|e| AppError::Unsatisfiable {
                reason: "volume.uncrossable",
                message: format!("volume {} cannot cross: {e}", v.name),
            })?;
            let size = (v.size_bytes, v.helper_id.clone());
            out.push(MigrationVolume::new(v.id, v.name.clone(), how, size));
        }
        Ok(out)
    }
}
