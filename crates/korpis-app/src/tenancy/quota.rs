// SPDX-License-Identifier: AGPL-3.0-or-later

//! Setting quota (§5.1 of docs/05-scheduling.md, §3.1 of docs/01-model.md). An organization's
//! quota set is its parent's decision, so it is set from above, as a suspension is: under
//! `org.quota.write` over the parent, which a grant at the organization itself never covers, so
//! no tenant raises its own. A root organization, with nothing above it, holds no quota: what it
//! holds is the installation's, and Korpis restricts no host (P10). A project's set is a
//! sub-limit within its organization's quota, set under `org.quota.write` over the organization.
//!
//! The set is checked under the locks of every set it is checked against: the reservations given
//! to an organization and its siblings, entry by entry of one shape, sum within their parent's,
//! never oversubscribed; their limits within the parent's times the overcommit it was permitted;
//! what the organization gives its own children fits what it is now given; and each of its
//! projects' sub-limits fits it. Lowering a set below what its workloads hold is allowed: only
//! growth is refused at admission, so nothing is stranded (§5.1). An entry whose shape the new set
//! keeps keeps its identity and its totals.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, QuotaSetWrite, RecipeSource, Store};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::tenancy::quota::{QuotaEntry, QuotaOwner, QuotaSet, SetError, shape};
use korpis_core::tenancy::{Organization, Project};
use std::collections::BTreeMap;
use uuid::Uuid;

const OPERATION: &str = "tenancy.set_quota";

/// A quota set to set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetQuota {
    /// The organization whose set it is, or whose project's.
    pub organization: Uuid,
    /// The project whose set it is, if it is a project's.
    pub project: Option<Uuid>,
    /// The set, replacing the one there.
    pub quota: QuotaSet,
}

/// A quota set as it now stands, with its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaSetAt {
    /// The organization whose set it is, or whose project's.
    pub organization: Organization,
    /// The project whose set it is, if it is a project's.
    pub project: Option<Project>,
    /// The set.
    pub quota: QuotaSet,
}

fn refused(e: &SetError) -> AppError {
    match e {
        SetError::Overallocated { .. } => AppError::conflict("quota.overallocated", e.to_string()),
        _ => AppError::invalid("quota", e.to_string()),
    }
}

/// The sets of the owners `entries` belong to, with the overcommit of those in `overcommit`.
fn grouped(
    entries: Vec<QuotaEntry>,
    overcommit: BTreeMap<Uuid, BTreeMap<korpis_core::tenancy::quota::Dimension, u32>>,
) -> BTreeMap<Uuid, QuotaSet> {
    let mut sets: BTreeMap<Uuid, QuotaSet> = BTreeMap::new();
    for e in entries {
        let id = match e.owner {
            QuotaOwner::Organization(o) => o,
            QuotaOwner::Project(p) => p,
        };
        sets.entry(id).or_default().entries.push(e);
    }
    for (id, percent) in overcommit {
        sets.entry(id).or_default().child_overcommit = percent;
    }
    sets
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The quota set of `owner`, with what its entries' workloads hold; read after the caller
    /// was shown it may read the owner.
    pub async fn quota_set(&self, owner: QuotaOwner) -> Result<QuotaSet, AppError> {
        Ok(self.store.quota_set(owner).await?)
    }

    /// The owner `cmd` sets the set of, the organization it is authorized over, and the
    /// organization and project themselves.
    async fn quota_owner(
        &self,
        caller: &Caller,
        cmd: &SetQuota,
    ) -> Result<(QuotaOwner, Uuid, Organization, Option<Project>), AppError> {
        let org = self.store.organization(cmd.organization).await?;
        let Some(org) = org else {
            self.authorize(caller, "org.quota.write", organization(cmd.organization)).await?;
            return Err(AppError::NotFound { kind: "organization" });
        };
        if let Some(p) = cmd.project {
            // Authorized before the project is looked for, so whether it exists is told only to
            // a caller who may set its quota.
            self.authorize(caller, "org.quota.write", organization(org.id)).await?;
            let project = self.store.project(p).await?.filter(|p| p.organization == org.id);
            let project = project.ok_or(AppError::NotFound { kind: "project" })?;
            return Ok((QuotaOwner::Project(p), org.id, org, Some(project)));
        }
        let Some(parent) = org.parent else {
            self.authorize(caller, "org.quota.write", organization(org.id)).await?;
            let why = "a root organization holds no quota: what it holds is the installation's";
            return Err(AppError::invalid("organization", why));
        };
        Ok((QuotaOwner::Organization(org.id), parent, org, None))
    }

    /// Replaces a quota set, from above.
    pub async fn set_quota(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, QuotaSetAt>,
        cmd: SetQuota,
    ) -> Result<Reply<QuotaSetAt>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let (owner, over, org, project) = self.quota_owner(caller, &cmd).await?;
        let permit = self.authorize(caller, "org.quota.write", organization(over)).await?;
        cmd.quota.validate().map_err(|e| refused(&e))?;
        if project.is_some() && !cmd.quota.child_overcommit.is_empty() {
            let why = "a project has no children to permit overcommit to";
            return Err(AppError::invalid("quota.child_overcommit_percent", why));
        }
        let mut tx = self.store.begin().await?;
        let current = self.check_quota(&mut tx, owner, over, &cmd.quota).await?;
        let quota = write_set(&mut tx, owner, current, cmd.quota).await?;
        let effect = caller
            .effect("org.quota.write", Outcome::Succeeded)
            .under(permit.grant)
            .after(details([
                ("organization", org.id.to_string().into()),
                ("project", project.as_ref().map(|p| p.id.to_string()).into()),
                ("entries", quota.entries.len().into()),
            ]));
        tx.append_effect(&effect).await?;
        let at = QuotaSetAt { organization: org, project, quota };
        finish(tx, caller, idem, OPERATION, at).await
    }

    /// Locks the set of `owner`, the set of `over` above it, and the sets beside and below it,
    /// and checks `new` against them; the entries `owner` has now.
    async fn check_quota(
        &self,
        tx: &mut S::Tx,
        owner: QuotaOwner,
        over: Uuid,
        new: &QuotaSet,
    ) -> Result<Vec<QuotaEntry>, AppError> {
        // The organization above first, then the owner: every change to a set takes the rows of
        // the organizations it is checked under, top down, before reading anything, so a
        // sibling's set made meanwhile is read once it is committed, never missed.
        let mut owners = vec![over];
        if let QuotaOwner::Organization(o) = owner {
            owners.push(o);
        }
        tx.lock_quota_owners(&owners).await?;
        let (beside, projects) = tx.quota_children(over).await?;
        let (below, below_projects) = match owner {
            QuotaOwner::Organization(o) => tx.quota_children(o).await?,
            QuotaOwner::Project(_) => (Vec::new(), Vec::new()),
        };
        let orgs: Vec<Uuid> =
            std::iter::once(over).chain(beside.clone()).chain(below.clone()).collect();
        let all_projects: Vec<Uuid> = projects.into_iter().chain(below_projects.clone()).collect();
        let entries = tx.lock_quota_sets(&orgs, &all_projects).await?;
        let mut percent = BTreeMap::new();
        percent.insert(over, tx.overcommit(QuotaOwner::Organization(over)).await?);
        let sets = grouped(entries, percent);
        let none = QuotaSet::default();
        let set = |id: &Uuid| sets.get(id).unwrap_or(&none);
        let above = set(&over);
        match owner {
            QuotaOwner::Project(p) => {
                above.bounds_project(new).map_err(|e| refused(&e))?;
                Ok(set(&p).entries.clone())
            }
            QuotaOwner::Organization(o) => {
                let mut given: Vec<&QuotaSet> =
                    beside.iter().filter(|b| **b != o).map(set).collect();
                given.push(new);
                above.fits(&given).map_err(|e| refused(&e))?;
                let children: Vec<&QuotaSet> = below.iter().map(set).collect();
                new.fits(&children).map_err(|e| refused(&e))?;
                for p in &below_projects {
                    new.bounds_project(set(p)).map_err(|e| refused(&e))?;
                }
                Ok(set(&o).entries.clone())
            }
        }
    }
}

fn organization(id: Uuid) -> ObjectRef {
    ObjectRef::new(Kind::Organization, id)
}

/// Writes `new` over `current`, the entries `owner` has: an entry of a shape kept keeps its
/// identity and totals and takes the new bounds, one of a new shape is added unreckoned, and one
/// whose shape is gone is removed. The set as it now stands.
async fn write_set<T: QuotaSetWrite>(
    tx: &mut T,
    owner: QuotaOwner,
    current: Vec<QuotaEntry>,
    new: QuotaSet,
) -> Result<QuotaSet, AppError> {
    let mut entries = Vec::with_capacity(new.entries.len());
    for mut e in new.entries {
        match current.iter().find(|c| shape(c) == shape(&e)) {
            Some(c) if c.reservations == e.reservations && c.limits == e.limits => {
                e = c.clone();
            }
            Some(c) => {
                tx.update_quota_bounds(c.id, &e.reservations, &e.limits).await?;
                e = QuotaEntry { version: c.version + 1, ..c.clone() }.with_bounds(e);
            }
            None => {
                e.id = Uuid::now_v7();
                e.owner = owner;
                e.version = 1;
                tx.insert_quota_entry(&e).await?;
            }
        }
        entries.push(e);
    }
    for gone in current.iter().filter(|c| !entries.iter().any(|e| e.id == c.id)) {
        tx.delete_quota_entry(gone.id).await?;
    }
    if let QuotaOwner::Organization(_) = owner {
        tx.put_overcommit(owner, &new.child_overcommit).await?;
    }
    Ok(QuotaSet { entries, child_overcommit: new.child_overcommit })
}
