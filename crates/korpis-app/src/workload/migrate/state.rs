// SPDX-License-Identifier: AGPL-3.0-or-later

//! A live migration's state volume. The workload's execution state crosses as its volumes do: in a
//! volume of its own, of kind `state`, which the control plane declares on the source as the
//! migration begins, in the class [`state_class`] chooses, so its row, its passes, its manifest at
//! `verify` and its deletion with the source's copies are every other volume's. It holds nothing
//! until `quiesce`, when the source's driver writes the state into it, and `final_sync` carries it.
//! It asks for no size, since the state is as large as the guest's memory and the drives it holds,
//! which only the driver knows; it is held for the migration alone.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store, VolumeWrite as _};
use korpis_core::node::Node;
use korpis_core::node::snapshot::state_class;
use korpis_core::workload::Workload;
use korpis_core::workload::migration::MigrationVolume;
use korpis_core::workload::volume::{
    LIVE_STATE, ReportedVolume, VolumeEvent, VolumeKind, live_state_id,
};
use std::time::SystemTime;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The state volume of `w`'s live migration from `source` to `destination`, declared on the
    /// source at `now`, as it crosses.
    pub(super) async fn state_volume(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
        (source, destination): (&Node, &Node),
        now: SystemTime,
    ) -> Result<MigrationVolume, AppError> {
        let (class, how) = state_class(source, destination).ok_or_else(|| {
            let why = "no class on the source holds the workload's state and crosses";
            AppError::conflict("migration.unrestorable", why)
        })?;
        let helper_id = live_state_id(w.id);
        let declared = ReportedVolume {
            workload: w.id,
            name: LIVE_STATE.to_owned(),
            helper_id: helper_id.clone(),
            storage_class: class.name.clone(),
            size_bytes: 0,
            quota_enforced: false,
            event: VolumeEvent::Ensured,
            kind: VolumeKind::State,
        };
        let id = tx.declare_volume(source.id, &declared, now).await?;
        Ok(MigrationVolume::new(id, LIVE_STATE.to_owned(), how, (0, helper_id)))
    }
}
