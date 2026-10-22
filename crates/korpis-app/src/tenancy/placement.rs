// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placement policies per organization (§2.2 of docs/05-scheduling.md, question 8 of §10). An
//! organization sets one for its subtree under `org.write`; a node is scored by the strategy and
//! weights of the policy nearest its owner, and a workload placed under the spread and affinity
//! of the policy nearest its own organization, each the installation's where no organization on
//! the way set one. A policy is taken whole from where it was set, never merged.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite as _, RecipeSource, Store, TenancyWrite as _};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::schedule::{PlacementPolicy, Policies, Strategy};
use std::collections::BTreeMap;
use uuid::Uuid;

const OPERATION: &str = "tenancy.set_placement_policy";

/// The placement policy at an organization.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyAt {
    /// The one it sets itself, if it does.
    pub own: Option<PlacementPolicy>,
    /// The one in force there: its own, the nearest above it that set one, or the
    /// installation's.
    pub effective: PlacementPolicy,
    /// Where the one in force was set; `None` for the installation's.
    pub set_at: Option<Uuid>,
}

fn org(id: Uuid) -> ObjectRef {
    ObjectRef::new(Kind::Organization, id)
}

const fn strategy(s: Strategy) -> &'static str {
    match s {
        Strategy::Packed => "packed",
        Strategy::Distributed => "distributed",
        Strategy::Balanced => "balanced",
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The policy at `organization`, under `org.read` there.
    pub async fn placement_policy(
        &self,
        caller: &Caller,
        organization: Uuid,
    ) -> Result<PolicyAt, AppError> {
        self.authorize(caller, "org.read", org(organization)).await?;
        let chain = self.owners(organization).await?;
        let set = self.store.placement_policies(&chain).await?;
        Ok(self.policy_at(&chain, &set))
    }

    /// The policy at the first organization of `chain`, nearest first, from the policies `set`
    /// along it.
    fn policy_at(&self, chain: &[Uuid], set: &BTreeMap<Uuid, PlacementPolicy>) -> PolicyAt {
        let own = chain.first().and_then(|o| set.get(o)).cloned();
        let nearest = chain.iter().find_map(|o| set.get(o).map(|p| (*o, p)));
        PolicyAt {
            own,
            effective: nearest.map_or_else(|| self.policy.clone(), |(_, p)| p.clone()),
            set_at: nearest.map(|(o, _)| o),
        }
    }

    /// The policies a workload is placed by whose organization's chain is `owners`, nearest
    /// first: its own organization's for its spread and affinity, and each owner's for the
    /// strategy and weights its nodes are scored by.
    pub(crate) async fn policies(&self, owners: &[Uuid]) -> Result<Policies, AppError> {
        let set = self.store.placement_policies(owners).await?;
        let at = |i: usize| self.policy_at(owners.get(i..).unwrap_or_default(), &set).effective;
        Ok(Policies {
            workload: at(0),
            owners: owners.iter().enumerate().map(|(i, o)| (*o, at(i))).collect(),
        })
    }

    /// Sets, replaces, or with `None` removes the policy `organization` sets for its subtree,
    /// under `org.write` there, as an effect. It decides the placements made after it and moves
    /// no workload already placed.
    pub async fn set_placement_policy(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, PolicyAt>,
        (organization, policy): (Uuid, Option<PlacementPolicy>),
    ) -> Result<Reply<PolicyAt>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit = self.authorize(caller, "org.write", org(organization)).await?;
        let chain = self.owners(organization).await?;
        let mut tx = self.store.begin().await?;
        let replaced = tx
            .set_placement_policy(organization, policy.as_ref())
            .await?
            .ok_or(AppError::NotFound { kind: "organization" })?;
        let effect = caller
            .effect("org.placement_policy.set", Outcome::Succeeded)
            .under(permit.grant)
            .about(org(organization))
            .after(details([
                ("organization", organization.to_string().into()),
                ("strategy", policy.as_ref().map(|p| strategy(p.strategy)).into()),
                ("replaced", replaced.is_some().into()),
            ]));
        tx.append_effect(&effect).await?;
        let mut set = self.store.placement_policies(chain.get(1..).unwrap_or_default()).await?;
        if let Some(p) = policy {
            set.insert(organization, p);
        }
        let at = self.policy_at(&chain, &set);
        finish(tx, caller, idem, OPERATION, at).await
    }
}
