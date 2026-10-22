// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations' themes (§7 of docs/11-surface-web.md): an organization sets its own, or wears
//! its nearest ancestor's, or the panel's own. A theme is what an organization shows everyone it
//! serves, so whoever may read an object may read the theme it is shown in; reading it spends no
//! use of a use-limited grant, since the page it styles spent the one it costs.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    Clock, EffectWrite, RecipeSource, SetTheme as Set, Store, ThemeChange, ThemeWrite,
};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Decision, Kind, ObjectRef};
use korpis_core::tenancy::theme::{Asked, Theme};
use uuid::Uuid;

const OPERATION: &str = "tenancy.set_theme";

/// The theme that applies to an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoverningTheme {
    /// The theme and the organization that set it; `None` when the panel's own applies.
    pub set: Option<Set>,
    /// For an organization asked about, the version of its own theme, zero when it has none;
    /// zero for anything else.
    pub own_version: u64,
}

/// A change to an organization's own theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetTheme {
    /// The organization.
    pub organization: Uuid,
    /// Its theme as asked; `None` removes its own, so its nearest ancestor's applies again.
    pub theme: Option<Asked>,
    /// The organization's own version as the caller saw it; zero if it has none.
    pub expected_version: u64,
}

fn org(id: Uuid) -> ObjectRef {
    ObjectRef::new(Kind::Organization, id)
}

fn checked(asked: &Asked) -> Result<Theme, AppError> {
    asked.checked().map_err(|e| AppError::invalid(format!("theme.{}", e.field), e.message))
}

fn effect_details(change: &ThemeChange) -> serde_json::Map<String, serde_json::Value> {
    let t = change.theme.as_ref();
    details([
        ("organization", change.organization.to_string().into()),
        ("version", change.version.into()),
        ("removed", change.theme.is_none().into()),
        ("product_name", t.map(|t| t.product_name.clone()).unwrap_or_default().into()),
        ("logo", t.is_some_and(|t| t.logo.is_some()).into()),
        ("stylesheet_bytes", t.map_or(0, |t| t.stylesheet.len()).into()),
    ])
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The theme that applies to `object`, an organization, a project, or a workload the caller
    /// may read.
    pub async fn get_theme(
        &self,
        caller: &Caller,
        object: ObjectRef,
    ) -> Result<GoverningTheme, AppError> {
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
        let set = self.store.theme(owner).await?;
        let own_version = if object.kind == Kind::Organization {
            self.store.own_theme_version(owner).await?
        } else {
            0
        };
        Ok(GoverningTheme { set, own_version })
    }

    /// The theme the sign-in page wears before anyone has signed in, answered to anyone: at a
    /// portal its organization's, and elsewhere the installation's root organization's (§7 of
    /// docs/11-surface-web.md).
    pub async fn sign_in_theme(&self, portal: Option<Uuid>) -> Result<GoverningTheme, AppError> {
        let wearer = match portal {
            Some(p) => self.portal_organization(p).await?,
            None => self.store.installation_root().await?,
        };
        let set = match wearer {
            Some(org) => self.store.theme(org).await?,
            None => None,
        };
        Ok(GoverningTheme { set, own_version: 0 })
    }

    /// Sets or removes `organization`'s own theme, which applies to it and to everything beneath
    /// it that sets none of its own.
    pub async fn set_theme(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, GoverningTheme>,
        cmd: SetTheme,
    ) -> Result<Reply<GoverningTheme>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit = self.authorize(caller, "org.write", org(cmd.organization)).await?;
        let organization = self
            .store
            .organization(cmd.organization)
            .await?
            .ok_or(AppError::NotFound { kind: "organization" })?;
        if cmd.theme.is_none() && cmd.expected_version == 0 {
            let message = "the organization has no theme of its own to remove";
            return Err(AppError::conflict("theme.not_set", message));
        }
        let change = ThemeChange {
            organization: organization.id,
            theme: cmd.theme.as_ref().map(checked).transpose()?,
            version: cmd.expected_version.saturating_add(1),
            set_by: caller.principal.clone(),
            set_at: self.clock.now(),
        };
        let set = match &change.theme {
            Some(theme) => Some(Set { organization: organization.id, theme: theme.clone() }),
            None => match organization.parent {
                Some(parent) => self.store.theme(parent).await?,
                None => None,
            },
        };
        let mut tx = self.store.begin().await?;
        if !tx.set_theme(&change, cmd.expected_version).await? {
            return Err(AppError::conflict(
                "theme.moved",
                "the organization's theme changed meanwhile",
            ));
        }
        let effect = caller
            .effect("org.theme.set", Outcome::Succeeded)
            .under(permit.grant)
            .about(ObjectRef::new(Kind::Organization, change.organization))
            .after(effect_details(&change));
        tx.append_effect(&effect).await?;
        let governing = GoverningTheme { set, own_version: change.version };
        finish(tx, caller, idem, OPERATION, governing).await
    }
}
