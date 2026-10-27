// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tending replicas (§2.2 of docs/06-storage.md): a pass over every volume on one node's disk
//! that a workload holds or that keeps replicas, choosing as many other nodes to keep one as the
//! workload's declaration of it asks for, as a placement chooses a node, and keeping them from
//! one pass to the next. A replica whose node no longer serves the volume's organization, or no
//! longer offers its class able to send increments, is dropped and another chosen; one that went
//! ten intervals without receiving is marked stale; a volume no declaration asks replicas of
//! keeps none. Each change is an effect, and each volume is tended in a transaction of its own,
//! under the lock every choice of replicas is made under. A transfer is due to each replica
//! reached that holds nothing yet, or what is an interval old, with none under way; those due in
//! one pass are sent one snapshot (`super::transfers`).

use crate::App;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{
    Clock, EffectWrite as _, PlacementWrite as _, RecipeSource, ReplicaWrite as _, Store, Tx as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::node::{Node, NodeStatus};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::tenancy::quota::{Bounds, Dimension};
use korpis_core::workload::Workload;
use korpis_core::workload::replica::{Tending, Wanted, tend};
use korpis_core::workload::volume::Volume;
use std::collections::BTreeMap;
use std::time::Duration;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// One pass over every volume whose replicas are tended; how many replicas it chose, dropped,
    /// or marked stale.
    pub async fn tend_replicas(&self) -> Result<u64, AppError> {
        let mut page = Page::new(None, Page::DEFAULT);
        let mut changed = 0;
        loop {
            let volumes = self.store.replicable(page).await?;
            for v in &volumes {
                changed += self.tend_volume(v).await?;
            }
            match page.next(&volumes, |v| v.id) {
                Some(after) => page = Page::new(Some(after), page.size),
                None => return Ok(changed),
            }
        }
    }

    /// Tends the replicas of `v`; how many it changed.
    async fn tend_volume(&self, v: &Volume) -> Result<u64, AppError> {
        let Some(on) = v.node else { return Ok(0) };
        let Some(home) = self.store.node(on).await? else { return Ok(0) };
        let holder = self.replicas_asked(v).await?;
        let owners = match &holder {
            Some((w, _)) => self.owners(w.organization).await?,
            None => Vec::new(),
        };
        let domains = self.failure_domains(&owners).await?;
        let wanted =
            Wanted { on: &home, class: &v.storage_class, size: v.size_bytes, domains: &domains };
        let mut tx = self.store.begin().await?;
        tx.lock_replicas().await?;
        let kept = tx.replicas_in(v.id).await?;
        if kept.is_empty() && holder.is_none() {
            return Ok(0);
        }
        let nodes = tx.serving_nodes(&owners).await?;
        let ids: Vec<Uuid> = nodes.iter().map(|n| n.id).collect();
        let allocations = tx.allocations(&ids).await?;
        let serving: Vec<&Node> = nodes.iter().collect();
        let ready = ready_with_room(&nodes, &allocations, v.size_bytes);
        let want = holder.as_ref().map(|(_, asked)| *asked);
        let now = self.clock.now();
        let t = tend(&wanted, want, &kept, (&serving, &ready), now);
        for (node, change) in changes(&t) {
            match change {
                Change::Dropped => _ = tx.drop_replica(v.id, node).await?,
                Change::Stale => tx.mark_stale((v.id, node), now).await?,
                Change::Chosen => tx.choose_replica((v.id, node), now).await?,
            }
            let organization = holder.as_ref().map(|(w, _)| w.organization);
            tx.append_effect(&effect(v, (node, change), organization)).await?;
        }
        // One snapshot a pass, sent to each replica a transfer is due to.
        let snapshot = Uuid::now_v7();
        for node in &t.due {
            tx.send_replica((v.id, *node), snapshot, now).await?;
        }
        tx.commit().await?;
        Ok(u64::try_from(t.dropped.len() + t.stale.len() + t.chosen.len()).unwrap_or(u64::MAX))
    }

    /// The workload holding `v` and how many replicas at what interval its declaration of `v`
    /// asks for, if it asks for any.
    async fn replicas_asked(
        &self,
        v: &Volume,
    ) -> Result<Option<(Workload, (usize, Duration))>, AppError> {
        let Some(id) = v.attached_to else { return Ok(None) };
        let Some(w) = self.store.workload(id).await?.filter(|w| w.deleted_at.is_none()) else {
            return Ok(None);
        };
        let Some(target) = w.target else { return Ok(None) };
        let Some(intent) = self.store.intent(target).await? else { return Ok(None) };
        let spec = intent.body.volumes.iter().find(|s| match s.existing {
            Some(existing) => existing == v.id,
            None => s.name == v.name,
        });
        let Some(r) = spec.and_then(|s| s.replication) else { return Ok(None) };
        let count = usize::try_from(r.replicas).unwrap_or(usize::MAX);
        Ok(Some((w, (count, r.interval))))
    }

    /// The labels whose values are failure domains for a workload of the organization whose
    /// chain is `owners`: those its policy spreads by, then `region`. `host` is not one: every
    /// replica is on another node already.
    async fn failure_domains(&self, owners: &[Uuid]) -> Result<Vec<String>, AppError> {
        let spread = match owners {
            [] => Vec::new(),
            _ => self.policies(owners).await?.workload.spread,
        };
        let mut keys: Vec<String> = Vec::new();
        for key in spread.into_iter().map(|s| s.topology_key).chain(["region".to_owned()]) {
            if key != "host" && !keys.contains(&key) {
                keys.push(key);
            }
        }
        Ok(keys)
    }
}

/// The `nodes` ready, with `size` bytes free on their disk beyond what is `allocated` there.
fn ready_with_room<'a>(
    nodes: &'a [Node],
    allocated: &BTreeMap<Uuid, Bounds>,
    size: u64,
) -> Vec<&'a Node> {
    let room = |n: &Node| {
        let taken = allocated.get(&n.id).and_then(|a| a.get(&Dimension::Disk)).copied();
        n.allocatable
            .get(&Dimension::Disk)
            .is_none_or(|total| total.saturating_sub(taken.unwrap_or(0)) >= size)
    };
    nodes.iter().filter(|n| n.status() == NodeStatus::Ready && room(n)).collect()
}

/// One change a pass makes to a replica.
#[derive(Debug, Clone, Copy)]
enum Change {
    Dropped,
    Stale,
    Chosen,
}

impl Change {
    /// The action its effect is recorded under.
    const fn action(self) -> &'static str {
        match self {
            Self::Dropped => "volume.replica_dropped",
            Self::Stale => "volume.replica_stale",
            Self::Chosen => "volume.replica_chosen",
        }
    }
}

/// Each change `t` makes, by node.
fn changes(t: &Tending) -> impl Iterator<Item = (Uuid, Change)> + '_ {
    let dropped = t.dropped.iter().map(|n| (*n, Change::Dropped));
    let stale = t.stale.iter().map(|n| (*n, Change::Stale));
    let chosen = t.chosen.iter().map(|n| (*n, Change::Chosen));
    dropped.chain(stale).chain(chosen)
}

/// The effect of `change` to `v`'s replica on `node`, made by the reconciler.
fn effect(v: &Volume, (node, change): (Uuid, Change), organization: Option<Uuid>) -> Effect {
    let actor = Principal::System(SystemActor::Reconciler);
    let mut e = Effect::new(actor, change.action(), Outcome::Succeeded)
        .after(details([("volume", v.id.to_string().into()), ("node", node.to_string().into())]));
    e.workload = v.attached_to;
    e.node = Some(node);
    e.object = Some(ObjectRef::new(Kind::Volume, v.id));
    e.organization = organization;
    e
}
