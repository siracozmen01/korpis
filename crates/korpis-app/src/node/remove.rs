// SPDX-License-Identifier: AGPL-3.0-or-later

//! Removing a node (§3 of docs/18-operations.md, §9 of docs/17-security.md). A node is removed
//! once it is cordoned and holds nothing: no workload placed on it and no volume not deleted,
//! since nothing moves a volume off a node before migration. One removed as compromised is
//! removed whatever it holds: each workload placed on it is placed again from its target as any
//! workload is, with the node no longer among those that serve, and its endpoints there are
//! dropped; its volumes stay where they are, lost with the node, and a workload attached to one
//! stays unplaced, saying why, until it is restored from a backup elsewhere.
//!
//! Either way the node is decommissioned, for good: its key opens no session, the session it
//! holds ends on whichever replica holds it, and its leases are fenced by the clocks, as any
//! silent node's are (§4.5 of docs/02-architecture.md): it cannot renew them, so each ends within
//! one term, and the next issue, to another node, raises the epoch.

use super::{NodeView, admin_name};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    CertificateWrite as _, Clock, EffectWrite as _, NodeWrite as _, PlacementWrite as _,
    RecipeSource, Store, WorkloadWrite as _,
};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::node::NodeAdmin;
use uuid::Uuid;

/// The operation a removal's idempotency key is kept under.
const OPERATION: &str = "node.remove";

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Removes node `id`, as `compromised` or not. Removing a node already removed changes
    /// nothing and records nothing.
    pub async fn remove_node(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, NodeView>,
        id: Uuid,
        compromised: bool,
    ) -> Result<Reply<NodeView>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit = self.authorize(caller, "node.remove", ObjectRef::new(Kind::Node, id)).await?;
        let mut tx = self.store.begin().await?;
        let locked = self.lock_placed(&mut tx, id).await?;
        let mut node = tx.lock_node(id).await?.ok_or(AppError::NotFound { kind: "node" })?;
        let from = node.admin;
        let changed = node.decommission(compromised).map_err(|e| {
            let why = format!("{e}: a node is removed once it is cordoned and empty");
            AppError::conflict("node.not_cordoned", why)
        })?;
        if changed {
            let (placed, volumes) = tx.node_holdings(id).await?;
            if !compromised && (!placed.is_empty() || volumes > 0) {
                let why = format!(
                    "it holds {} workloads and {volumes} volumes; stop or delete them first, or \
                     remove it as compromised",
                    placed.len()
                );
                return Err(AppError::conflict("node.not_empty", why));
            }
            if !tx.update_node_admin(&node, node.version).await? {
                return Err(AppError::Internal(format!("node {id} moved while locked")));
            }
            node.version += 1;
            let by = (caller, permit.grant);
            let held = (placed, volumes, locked.as_slice());
            self.decommissioned(&mut tx, by, (id, from, compromised), held).await?;
        }
        let allocated = tx.allocations(&[id]).await?.remove(&id).unwrap_or_default();
        finish(tx, caller, idem, OPERATION, NodeView { node, allocated }).await
    }

    /// The workloads placed on node `id`, each locked, and then the placement lock: the order
    /// every placement takes them in, so placing them again cannot deadlock with a placement of
    /// one of them.
    async fn lock_placed(&self, tx: &mut S::Tx, id: Uuid) -> Result<Vec<Uuid>, AppError> {
        let (mut placed, _) = tx.node_holdings(id).await?;
        placed.sort_unstable();
        for w in &placed {
            tx.lock_workload(*w).await?;
        }
        tx.lock_placement().await?;
        Ok(placed)
    }

    /// What decommissioning node `id` does beyond its own row: each workload on it placed again,
    /// each locked before the placement lock was (`locked`), its endpoints dropped, its
    /// certificates exposed if it was compromised, and the effect recording all of it. A workload
    /// placed on it after its workloads were locked is a conflict: the removal is asked again.
    async fn decommissioned(
        &self,
        tx: &mut S::Tx,
        (caller, grant): (&Caller, Uuid),
        (id, from, compromised): (Uuid, NodeAdmin, bool),
        (placed, volumes, locked): (Vec<Uuid>, u64, &[Uuid]),
    ) -> Result<(), AppError> {
        let mut moved = 0usize;
        for w in placed {
            if !locked.contains(&w) {
                let why = "a workload was placed on it while it was being removed; ask again";
                return Err(AppError::conflict("node.placed_meanwhile", why));
            }
            moved += usize::from(self.displace(tx, w, id).await?);
        }
        let endpoints = tx.drop_node_endpoints(id).await?;
        let exposed =
            if compromised { tx.expose_certificates(id, self.clock.now()).await? } else { 0 };
        tx.announce_resync().await?;
        let mut effect =
            caller.effect("node.remove", Outcome::Succeeded).under(grant).after(details([
                ("node", id.to_string().into()),
                ("from", admin_name(from).into()),
                ("compromised", compromised.into()),
                ("workloads_placed_again", moved.into()),
                ("endpoints_dropped", endpoints.into()),
                ("volumes_lost", volumes.into()),
                ("certificates_exposed", exposed.into()),
            ]));
        effect.node = Some(id);
        tx.append_effect(&effect).await?;
        Ok(())
    }
}
