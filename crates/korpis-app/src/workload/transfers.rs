// SPDX-License-Identifier: AGPL-3.0-or-later

//! Transfers between a volume's node and its replicas' (§2.2 of docs/06-storage.md): what each
//! node is told of them, every transfer with a ticket signed by the token key, and what a
//! replica's node reports of a receive. A report is believed only of the transfer under way to
//! the node that sends it, of the snapshot it names: nothing else a node says moves a replica.

use crate::App;
use crate::error::AppError;
use crate::node::journal::rejected;
use crate::ports::{Clock, RecipeSource, ReplicaWrite as _, Store};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::token::SigningKey;
use korpis_core::authz::transfer::{Transfer, mint};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::replica::{ReplicaEvent, ReportedReplica};
use korpis_core::workload::transfers::{NodeReplicas, Outgoing, told};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The action a receive is recorded under.
const RECEIVE: &str = "volume.replica_receive";

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// What each node is told of replicas now, by node, each transfer's ticket signed.
    pub async fn replication(&self) -> Result<BTreeMap<Uuid, NodeReplicas>, AppError> {
        let mut out = told(&self.store.flows().await?);
        for (node, n) in &mut out {
            for o in &mut n.sending {
                sign(&self.token_key, *node, o);
            }
        }
        Ok(out)
    }

    /// Records what `node` reports of a receive into its replica: the effect that records it,
    /// or a rejection when no transfer of that snapshot to it is under way.
    pub(crate) async fn replica_event(
        &self,
        tx: &mut S::Tx,
        node: Uuid,
        r: &ReportedReplica,
    ) -> Result<Effect, AppError> {
        let replica = (r.volume, node);
        let (recorded, outcome) = match &r.event {
            ReplicaEvent::Received => {
                let at = self.clock.now();
                (tx.replica_received(replica, r.snapshot, at).await?, Outcome::Succeeded)
            }
            ReplicaEvent::Failed(_) => {
                (tx.replica_failed(replica, r.snapshot).await?, Outcome::Failed)
            }
        };
        if !recorded {
            return Ok(rejected("no transfer of that snapshot to the node is under way", RECEIVE));
        }
        let mut e = Effect::new(Principal::System(SystemActor::Reconciler), RECEIVE, outcome)
            .after(details([
                ("volume", r.volume.to_string().into()),
                ("snapshot", r.snapshot.to_string().into()),
            ]));
        if let ReplicaEvent::Failed(why) = &r.event {
            e = e.error(details([("reason", why.clone().into())]));
        }
        e.object = Some(ObjectRef::new(Kind::Volume, r.volume));
        Ok(e)
    }
}

/// Signs each transfer of `o`, a volume on `from`, with `key`.
fn sign(key: &SigningKey, from: Uuid, o: &mut Outgoing) {
    for t in &mut o.transfers {
        let transfer = Transfer {
            volume: o.volume,
            from,
            to: t.to,
            snapshot: t.snapshot,
            parent: t.parent,
            expires_at: t.expires_at,
        };
        t.ticket = mint(key, &transfer);
    }
}
