// SPDX-License-Identifier: AGPL-3.0-or-later

//! Attaching an existing volume (§7 of docs/06-storage.md): a declaration names it by its
//! identifier, and the control plane resolves it. Declaring checks that it is a live volume the
//! workload's project holds, that the declarer may read what is on it, and that no other workload
//! holds it; approving claims it in the same transaction that moves the target, so two workloads
//! are never both approved to one volume; placement keeps the workload on the node whose disk
//! holds it; and the node is told the helper's identifier for it, as its node reported it.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store, VolumeWrite as _};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::volume::VolumeKind;
use korpis_core::workload::{IntentBody, Workload};
use std::collections::BTreeMap;
use uuid::Uuid;

fn field(i: usize) -> String {
    format!("volumes[{i}].existing")
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Checks each existing volume `body` attaches to `workload` of `project`.
    pub(super) async fn check_existing(
        &self,
        caller: &Caller,
        (workload, project): (Uuid, Uuid),
        body: &IntentBody,
    ) -> Result<(), AppError> {
        let mut seen = Vec::new();
        for (i, id) in body.volumes.iter().enumerate().filter_map(|(i, v)| Some((i, v.existing?))) {
            if seen.contains(&id) {
                return Err(AppError::invalid(field(i), "a volume is attached once"));
            }
            seen.push(id);
            let none = || AppError::invalid(field(i), "no such volume in the workload's project");
            let v =
                self.store.volume(id).await?.filter(|v| v.project == project).ok_or_else(none)?;
            // Attaching it shows the workload, and so its declarer, everything on it.
            self.authorize(caller, "volume.read", ObjectRef::new(Kind::Volume, id)).await?;
            if v.deleted_at.is_some() {
                return Err(AppError::invalid(field(i), "the volume is deleted"));
            }
            if v.kind != VolumeKind::Declared {
                return Err(AppError::invalid(field(i), "a console stream's volume is its own"));
            }
            if v.attached_to.is_some_and(|w| w != workload) {
                let why = format!("volume {id} is held by another workload");
                return Err(AppError::conflict("volume.attached", why));
            }
        }
        Ok(())
    }

    /// Claims for `w` each existing volume `body` attaches, as its target is approved; refused
    /// if another workload took one since it was declared.
    pub(crate) async fn claim_existing(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
        body: &IntentBody,
    ) -> Result<(), AppError> {
        for id in body.volumes.iter().filter_map(|v| v.existing) {
            if !tx.claim_volume(id, w.id).await? {
                let why = format!("volume {id} is held by another workload, or deleted");
                return Err(AppError::conflict("volume.attached", why));
            }
        }
        Ok(())
    }

    /// Each existing volume `body` attaches, with the node holding it.
    pub(super) async fn existing_nodes(
        &self,
        body: &IntentBody,
    ) -> Result<Vec<(Uuid, Uuid)>, AppError> {
        let mut out = Vec::new();
        for id in body.volumes.iter().filter_map(|v| v.existing) {
            let v = self.store.volume(id).await?.ok_or(AppError::NotFound { kind: "volume" })?;
            out.push((id, v.node));
        }
        Ok(out)
    }

    /// The helper's identifier of each existing volume `body` attaches, by the name the
    /// workload mounts it under.
    pub(crate) async fn existing_ids(
        &self,
        body: &IntentBody,
    ) -> Result<BTreeMap<String, String>, AppError> {
        let mut out = BTreeMap::new();
        for spec in &body.volumes {
            let Some(id) = spec.existing else { continue };
            let v = self.store.volume(id).await?.ok_or(AppError::NotFound { kind: "volume" })?;
            out.insert(spec.name.clone(), v.helper_id);
        }
        Ok(out)
    }
}
