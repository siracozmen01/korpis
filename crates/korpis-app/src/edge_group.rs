// SPDX-License-Identifier: AGPL-3.0-or-later

//! Edge groups (§3.1 of docs/07-networking.md, §3.6 of docs/01-model.md): made, changed, and
//! deleted by whoever may write them in their organization, their members that organization's
//! own nodes alone, since a member translates every connection to the addresses it holds.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{
    Clock, EdgeWrite as _, EffectWrite as _, PlacementWrite as _, RecipeSource, Store, StoreError,
    Unique,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::edge::EdgeGroup;
use std::time::Duration;
use uuid::Uuid;

mod ingress;
mod lease;
mod placed;
mod probe;
mod reach;
mod tables;

pub use lease::EdgeLeaseAnswer;
pub(crate) use placed::Addressing;
pub use probe::{ProbePlan, ProbeTarget};
pub use reach::EdgeReach;
pub use tables::EdgeTables;

/// A group as its maker asks for it: everything but its id and creation time, each setting
/// left out taking its default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeGroupSpec {
    /// Its organization.
    pub organization: Uuid,
    /// Its name.
    pub name: String,
    /// Its members.
    pub members: Vec<Uuid>,
    /// The ports given to `stable` endpoints.
    pub ports: Option<(u16, u16)>,
    /// The port forwarded for the probe.
    pub probe_port: Option<u16>,
    /// Between two probes of a member.
    pub probe_interval: Option<Duration>,
    /// Missed in a row before a member is failing.
    pub probe_misses: Option<u32>,
    /// A member's lease on an address.
    pub term: Option<Duration>,
}

impl EdgeGroupSpec {
    fn group(self, id: Uuid, created_at: std::time::SystemTime) -> EdgeGroup {
        use korpis_core::edge as e;
        let mut members = self.members;
        members.sort_unstable();
        EdgeGroup {
            id,
            organization: self.organization,
            name: self.name,
            members,
            ports: self.ports.unwrap_or(e::DEFAULT_PORTS),
            probe_port: self.probe_port.unwrap_or(e::DEFAULT_PROBE_PORT),
            probe_interval: self.probe_interval.unwrap_or(e::DEFAULT_PROBE_INTERVAL),
            probe_misses: self.probe_misses.unwrap_or(e::DEFAULT_PROBE_MISSES),
            term: self.term.unwrap_or(e::DEFAULT_TERM),
            created_at,
        }
    }
}

fn effect(caller: &Caller, action: &str, grant: Uuid, g: &EdgeGroup) -> Effect {
    let about = ObjectRef::new(Kind::Organization, g.organization);
    let members: Vec<String> = g.members.iter().map(ToString::to_string).collect();
    caller.effect(action, Outcome::Succeeded).under(grant).about(about).after(details([
        ("group", g.id.to_string().into()),
        ("name", g.name.clone().into()),
        ("members", members.join(",").into()),
    ]))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Refuses a group that could not hold what it says: its settings, and members that are not
    /// its organization's own nodes.
    async fn group_holdable(&self, g: &EdgeGroup) -> Result<(), AppError> {
        g.validate().map_err(|e| AppError::invalid("group", e.to_string()))?;
        for m in &g.members {
            let found = self.store.node(*m).await?;
            if found.is_none_or(|n| n.organization != g.organization) {
                return Err(AppError::invalid(
                    "group.members",
                    "a group's members are its organization's own nodes, since a member \
                     translates every connection to an address it holds",
                ));
            }
        }
        Ok(())
    }

    /// Makes an edge group.
    pub async fn create_edge_group(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, EdgeGroup>,
        spec: EdgeGroupSpec,
    ) -> Result<Reply<EdgeGroup>, AppError> {
        const OPERATION: &str = "edge_group.create";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let org = ObjectRef::new(Kind::Organization, spec.organization);
        let permit = self.authorize(caller, "edge_group.write", org).await?;
        let group = spec.group(Uuid::now_v7(), self.clock.now());
        self.group_holdable(&group).await?;
        let mut tx = self.store.begin().await?;
        tx.insert_edge_group(&group).await.map_err(|e| match e {
            StoreError::Duplicate(Unique::EdgeGroupName) => {
                AppError::conflict("edge_group.name_taken", "its organization has a group so named")
            }
            other => other.into(),
        })?;
        tx.append_effect(&effect(caller, OPERATION, permit.grant, &group)).await?;
        finish(tx, caller, idem, OPERATION, group).await
    }

    /// One group.
    pub async fn edge_group(&self, caller: &Caller, id: Uuid) -> Result<EdgeGroup, AppError> {
        let missing = AppError::NotFound { kind: "edge group" };
        let group = self.store.edge_group(id).await?.ok_or(missing)?;
        let org = ObjectRef::new(Kind::Organization, group.organization);
        self.authorize(caller, "edge_group.read", org).await?;
        Ok(group)
    }

    /// A page of an organization's groups.
    pub async fn list_edge_groups(
        &self,
        caller: &Caller,
        organization: Uuid,
        page: Page,
    ) -> Result<Paged<EdgeGroup>, AppError> {
        let org = ObjectRef::new(Kind::Organization, organization);
        self.authorize(caller, "edge_group.read", org).await?;
        let items = self.store.edge_groups_of(organization, page).await?;
        let next = page.next(&items, |g| g.id);
        Ok(Paged { items, next })
    }

    /// Replaces a group's members and settings, under the placement lock, since which nodes
    /// a group's workloads may be placed on follows its members.
    pub async fn update_edge_group(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, EdgeGroup>,
        (id, spec): (Uuid, EdgeGroupSpec),
    ) -> Result<Reply<EdgeGroup>, AppError> {
        const OPERATION: &str = "edge_group.update";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let missing = || AppError::NotFound { kind: "edge group" };
        let before = self.store.edge_group(id).await?.ok_or_else(missing)?;
        let org = ObjectRef::new(Kind::Organization, before.organization);
        let permit = self.authorize(caller, "edge_group.write", org).await?;
        let asked = EdgeGroupSpec { organization: before.organization, name: before.name, ..spec };
        let group = asked.group(id, before.created_at);
        self.group_holdable(&group).await?;
        let mut tx = self.store.begin().await?;
        tx.lock_placement().await?;
        if !tx.update_edge_group(&group).await? {
            return Err(missing());
        }
        tx.append_effect(&effect(caller, OPERATION, permit.grant, &group)).await?;
        finish(tx, caller, idem, OPERATION, group).await
    }

    /// Deletes a group no address pool names.
    pub async fn delete_edge_group(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, ()>,
        id: Uuid,
    ) -> Result<Reply<()>, AppError> {
        const OPERATION: &str = "edge_group.delete";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let missing = AppError::NotFound { kind: "edge group" };
        let group = self.store.edge_group(id).await?.ok_or(missing)?;
        let org = ObjectRef::new(Kind::Organization, group.organization);
        let permit = self.authorize(caller, "edge_group.write", org).await?;
        if self.store.edge_group_named(id).await? {
            let why = "an address pool names it; delete its pools first";
            return Err(AppError::conflict("edge_group.named", why));
        }
        let mut tx = self.store.begin().await?;
        tx.delete_edge_group(id).await?;
        tx.append_effect(&effect(caller, OPERATION, permit.grant, &group)).await?;
        finish(tx, caller, idem, OPERATION, ()).await
    }
}
