// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations' layouts (question 2 of §10 of docs/11-surface-web.md): the landing screen an
//! organization composes for those it serves, its own, or its nearest ancestor's, or the panel's
//! own. A layout is shown to everyone the organization serves and places only components that
//! render under the viewer's own authority, so whoever may read an object may read the layout
//! that applies to it, as for a theme; reading it spends no use of a use-limited grant.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    Clock, EffectWrite, LayoutChange, LayoutWrite, RecipeSource, SetLayout as Set, Store,
};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Decision, Kind, ObjectRef};
use korpis_core::tenancy::layout::{Asked, Layout};
use uuid::Uuid;

const OPERATION: &str = "tenancy.set_layout";

/// The layout that applies to an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoverningLayout {
    /// The layout and the organization that set it; `None` when the panel's own applies.
    pub set: Option<Set>,
    /// For an organization asked about, the version of its own layout, zero when it has none;
    /// zero for anything else.
    pub own_version: u64,
}

/// A change to an organization's own layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetLayout {
    /// The organization.
    pub organization: Uuid,
    /// Its layout as asked; `None` removes its own, so its nearest ancestor's applies again.
    pub layout: Option<Asked>,
    /// The organization's own version as the caller saw it; zero if it has none.
    pub expected_version: u64,
}

fn checked(asked: &Asked) -> Result<Layout, AppError> {
    asked.checked().map_err(|e| AppError::invalid(format!("layout.{}", e.field), e.message))
}

fn effect_details(change: &LayoutChange) -> serde_json::Map<String, serde_json::Value> {
    let l = change.layout.as_ref();
    details([
        ("organization", change.organization.to_string().into()),
        ("version", change.version.into()),
        ("removed", change.layout.is_none().into()),
        ("name", l.map(|l| l.name.clone()).unwrap_or_default().into()),
        ("items", l.map_or(0, |l| l.items.len()).into()),
    ])
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The layout that applies to `object`, an organization, a project, or a workload the
    /// caller may read.
    pub async fn get_layout(
        &self,
        caller: &Caller,
        object: ObjectRef,
    ) -> Result<GoverningLayout, AppError> {
        let action = match object.kind {
            Kind::Organization => "org.read",
            Kind::Project => "project.read",
            Kind::Workload => "workload.read",
            _ => return Err(AppError::invalid("of", "an organization, a project, or a workload")),
        };
        let action = crate::authz::gateway::core_action(action)?;
        if !matches!(self.decide(caller, &action, object, None).await?, Decision::Permit(_)) {
            return Err(self.refuse(caller, &action, object).await);
        }
        let chain = self.store.ancestry(object).await?;
        let owner = chain
            .iter()
            .find(|n| n.object.kind == Kind::Organization)
            .map(|n| n.object.id)
            .ok_or(AppError::NotFound { kind: "organization" })?;
        let set = self.store.layout(owner).await?;
        let own_version = if object.kind == Kind::Organization {
            self.store.own_layout_version(owner).await?
        } else {
            0
        };
        Ok(GoverningLayout { set, own_version })
    }

    /// The layout `caller`'s landing shows, reached at `portal` or at the installation's own
    /// address: that of the organization they land at, whoever provides their access, though
    /// they may read none of its organizations (Finding 134 of docs/23-walkthroughs.md). Answered
    /// to any caller signed in, whatever they hold, since what it places renders under their own
    /// authority; a share link's holder is answered as anyone not signed in is.
    pub async fn landing_layout(
        &self,
        caller: &Caller,
        portal: Option<Uuid>,
    ) -> Result<GoverningLayout, AppError> {
        let set = match self.landing_organization(caller, portal).await? {
            Some(org) => self.store.layout(org).await?,
            None => None,
        };
        Ok(GoverningLayout { set, own_version: 0 })
    }

    /// Sets or removes `organization`'s own layout, which applies to it and to everything
    /// beneath it that sets none of its own.
    pub async fn set_layout(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, GoverningLayout>,
        cmd: SetLayout,
    ) -> Result<Reply<GoverningLayout>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let object = ObjectRef::new(Kind::Organization, cmd.organization);
        let permit = self.authorize(caller, "org.write", object).await?;
        let organization = self
            .store
            .organization(cmd.organization)
            .await?
            .ok_or(AppError::NotFound { kind: "organization" })?;
        if cmd.layout.is_none() && cmd.expected_version == 0 {
            let message = "the organization has no layout of its own to remove";
            return Err(AppError::conflict("layout.not_set", message));
        }
        let change = LayoutChange {
            organization: organization.id,
            layout: cmd.layout.as_ref().map(checked).transpose()?,
            version: cmd.expected_version.saturating_add(1),
            set_by: caller.principal.clone(),
            set_at: self.clock.now(),
        };
        let set = match &change.layout {
            Some(layout) => Some(Set { organization: organization.id, layout: layout.clone() }),
            None => match organization.parent {
                Some(parent) => self.store.layout(parent).await?,
                None => None,
            },
        };
        let mut tx = self.store.begin().await?;
        if !tx.set_layout(&change, cmd.expected_version).await? {
            let message = "the organization's layout changed meanwhile";
            return Err(AppError::conflict("layout.moved", message));
        }
        let effect = caller
            .effect("org.layout.set", Outcome::Succeeded)
            .under(permit.grant)
            .about(object)
            .after(effect_details(&change));
        tx.append_effect(&effect).await?;
        let governing = GoverningLayout { set, own_version: change.version };
        finish(tx, caller, idem, OPERATION, governing).await
    }
}
