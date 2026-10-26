// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nodes (§3.4 of docs/01-model.md): reading them, and the operator's cordon. A node is covered
//! by grants over the organization it was enrolled into.

mod assign;
mod attach;
mod endpoints;
mod enroll;

pub(crate) use enroll::{OneUse, enrollment_grant};
mod identity;
mod journal;
mod lease;
mod mesh;
mod observe;
mod reachability;
mod remove;
pub(crate) mod session;
mod told;

pub use assign::AssignmentSet;
pub use attach::check_data_hello;
pub(crate) use enroll::first_enrollment;
pub use enroll::{
    CreateEnrollment, DEFAULT_ENROLLMENT_TTL, Enroll, Enrolled, EnrollmentToken, MAX_ENROLLMENT_TTL,
};
pub use identity::{CredentialRefusal, IssuedCredential};
pub use lease::LeaseAnswer;
pub(crate) use lease::node_issue;
pub use reachability::ProbeDue;
pub use session::{AGENT_PROTOCOLS, Hello, NodeProof, Session, SessionTerms};

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{Clock, EffectWrite, NodeWrite, PlacementWrite, RecipeSource, Store};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::labels::LabelSelector;
use korpis_core::node::{Node, NodeAdmin};
use korpis_core::tenancy::quota::Bounds;
use uuid::Uuid;

const OPERATION: &str = "node.set_admin";

/// A node as a client renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeView {
    /// The node.
    pub node: Node,
    /// What the placements on it reserve.
    pub allocated: Bounds,
    /// Its place in the mesh: whether it reaches every other member, its side as its agent
    /// reported it, and each pair with it (§9.1 of docs/07-networking.md).
    pub overlay: korpis_core::node::mesh::Standing,
}

fn admin_name(a: NodeAdmin) -> &'static str {
    match a {
        NodeAdmin::Active => "active",
        NodeAdmin::Cordoned => "cordoned",
        NodeAdmin::Draining => "draining",
        NodeAdmin::Decommissioned => "decommissioned",
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    async fn node_view(&self, node: Node, judged: &mesh::Judged) -> Result<NodeView, AppError> {
        let allocated = self.store.allocated(node.id).await?;
        let overlay = judged.standing(node.id);
        Ok(NodeView { node, allocated, overlay })
    }

    /// One node the caller may read.
    pub async fn get_node(&self, caller: &Caller, id: Uuid) -> Result<NodeView, AppError> {
        self.authorize(caller, "node.read", ObjectRef::new(Kind::Node, id)).await?;
        let node = self.store.node(id).await?.ok_or(AppError::NotFound { kind: "node" })?;
        let mut view = self.node_view(node, &self.judged().await?).await?;
        self.readable_peers(caller, std::slice::from_mut(&mut view.overlay)).await?;
        Ok(view)
    }

    /// Leaves in each of `standings` only the peers `caller` may read: a pair is paired by the
    /// organizations above both nodes, and the caller may read one node and not the other.
    async fn readable_peers(
        &self,
        caller: &Caller,
        standings: &mut [korpis_core::node::mesh::Standing],
    ) -> Result<(), AppError> {
        let peers: std::collections::BTreeSet<Uuid> =
            standings.iter().flat_map(|s| s.peers.iter().map(|(p, _)| *p)).collect();
        let objects: Vec<ObjectRef> =
            peers.iter().map(|p| ObjectRef::new(Kind::Node, *p)).collect();
        let permitted = self.permitted_each(caller, "node.read", &objects).await?;
        let readable: std::collections::BTreeSet<Uuid> =
            peers.into_iter().zip(permitted).filter_map(|(p, ok)| ok.then_some(p)).collect();
        for s in standings {
            s.peers.retain(|(p, _)| readable.contains(p));
        }
        Ok(())
    }

    /// The nodes an organization owns, matching a selector. A page may hold fewer than its
    /// size when the selector skips some; its cursor continues after the last node read.
    pub async fn list_nodes(
        &self,
        caller: &Caller,
        organization: Uuid,
        selector: &LabelSelector,
        page: Page,
    ) -> Result<Paged<NodeView>, AppError> {
        let org = ObjectRef::new(Kind::Organization, organization);
        self.authorize(caller, "node.read", org).await?;
        let read = self.store.nodes(organization, page).await?;
        let next = page.next(&read, |n| n.id);
        let (mut items, judged) = (Vec::new(), self.judged().await?);
        for n in read.into_iter().filter(|n| selector.matches(n.labels.as_map())) {
            items.push(self.node_view(n, &judged).await?);
        }
        let mut standings: Vec<_> =
            items.iter_mut().map(|v| std::mem::take(&mut v.overlay)).collect();
        self.readable_peers(caller, &mut standings).await?;
        for (v, s) in items.iter_mut().zip(standings) {
            v.overlay = s;
        }
        Ok(Paged { items, next })
    }

    /// Cordons a node (`to` is [`NodeAdmin::Cordoned`]) or uncordons it
    /// ([`NodeAdmin::Active`]). Cordoning stops new placements and touches nothing running;
    /// it takes the placement lock, so no bind in flight lands on a node after it is cordoned.
    /// Asking for the state the node is already in changes nothing and records nothing.
    pub async fn set_node_admin(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, NodeView>,
        id: Uuid,
        to: NodeAdmin,
    ) -> Result<Reply<NodeView>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit = self.authorize(caller, "node.cordon", ObjectRef::new(Kind::Node, id)).await?;
        let mut overlay = self.judged().await?.standing(id);
        self.readable_peers(caller, std::slice::from_mut(&mut overlay)).await?;
        let mut tx = self.store.begin().await?;
        tx.lock_placement().await?;
        let mut node = tx.lock_node(id).await?.ok_or(AppError::NotFound { kind: "node" })?;
        let from = node.admin;
        let changed = node.set_admin(to).map_err(|e| {
            AppError::conflict("node.admin_transition", format!("{e}: its operation decides"))
        })?;
        if changed {
            if !tx.update_node_admin(&node, node.version).await? {
                return Err(AppError::Internal(format!("node {id} moved while locked")));
            }
            node.version += 1;
            let action = if to == NodeAdmin::Cordoned { "node.cordon" } else { "node.uncordon" };
            let mut effect =
                caller.effect(action, Outcome::Succeeded).under(permit.grant).after(details([
                    ("node", id.to_string().into()),
                    ("from", admin_name(from).into()),
                    ("to", admin_name(to).into()),
                ]));
            effect.node = Some(id);
            tx.append_effect(&effect).await?;
        }
        let allocated = tx.allocations(&[id]).await?.remove(&id).unwrap_or_default();
        finish(tx, caller, idem, OPERATION, NodeView { node, allocated, overlay }).await
    }
}
