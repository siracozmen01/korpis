// SPDX-License-Identifier: AGPL-3.0-or-later

//! Replicated volumes as they are declared (§2.2 of docs/06-storage.md): a volume asking for
//! replicas is accepted only in a class able to send increments, since a copy sent whole every
//! interval is not one that can be kept current (Rule K-3). The class is the one the volume
//! names, or each node's default; it is refused at declaration when no node serving the
//! workload's organization offers such a class, and placement keeps it off a node whose class
//! does not (§2.1 of docs/05-scheduling.md).

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, PlacementWrite as _, RecipeSource, Store};
use korpis_core::node::driver::{StorageFeature, class_named};
use korpis_core::workload::IntentBody;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Refuses a volume of `body`, declared for a workload of `organization`, that asks for
    /// replicas in a class no node serving it can send increments from.
    pub(super) async fn check_replicated(
        &self,
        organization: Uuid,
        body: &IntentBody,
    ) -> Result<(), AppError> {
        if body.volumes.iter().all(|v| v.replication.is_none()) {
            return Ok(());
        }
        let owners = self.owners(organization).await?;
        let nodes = {
            let mut tx = self.store.begin().await?;
            tx.serving_nodes(&owners).await?
        };
        for (i, v) in body.volumes.iter().enumerate().filter(|(_, v)| v.replication.is_some()) {
            let sends = nodes.iter().any(|n| {
                class_named(&n.storage_classes, &v.storage_class)
                    .is_some_and(|c| c.capabilities.has(StorageFeature::IncrementalSend))
            });
            if !sends {
                return Err(AppError::invalid(format!("volumes[{i}].replicas"), unable(v)));
            }
        }
        Ok(())
    }
}

/// Why `v`'s replicas are refused.
fn unable(v: &korpis_core::workload::spec::VolumeSpec) -> String {
    let class = if v.storage_class.is_empty() {
        "no node's default class".to_owned()
    } else {
        format!("no node's class {:?}", v.storage_class)
    };
    format!("{class} can send increments, which a replica is kept by")
}
