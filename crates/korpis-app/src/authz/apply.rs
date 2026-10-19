// SPDX-License-Identifier: AGPL-3.0-or-later

//! Issuing from a template (§4 of docs/08-identity.md): the template fills in the request, and
//! the issue that follows is the ordinary attenuation of a grant the issuer holds. A template
//! grants nobody anything; what it names that the issuer may not pass on is refused exactly as a
//! request written by hand would be.

use super::IssueGrant;
use crate::App;
use crate::authz::gateway::read_namespace;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Conditions, Delegation, Kind, ObjectRef, Scope, TemplateRef};
use uuid::Uuid;

/// The template a grant is issued from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FromTemplate {
    /// The template.
    pub template: Uuid,
    /// The version the issuer read: a template edited since is a conflict, never a silent
    /// substitution.
    pub version: u64,
    /// Conditions given at issue, in place of the template's defaults.
    pub conditions: Option<Conditions>,
}

fn not_found() -> AppError {
    AppError::NotFound { kind: "grant template" }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The organizations at and above `at`, nearest first.
    pub(crate) async fn organizations_above(&self, at: ObjectRef) -> Result<Vec<Uuid>, AppError> {
        let ancestry = self.store.ancestry(at).await?;
        Ok(ancestry
            .iter()
            .filter(|n| n.object.kind == Kind::Organization)
            .map(|n| n.object.id)
            .collect())
    }

    /// Fills in `cmd` from the template it names, if it names one: its actions, its delegation
    /// policy, and its scope from the pattern at the object `cmd.scope` names, with its default
    /// conditions unless others were given. The template's provenance, to record on the grant.
    pub(crate) async fn apply_template(
        &self,
        caller: &Caller,
        cmd: &mut IssueGrant,
    ) -> Result<Option<TemplateRef>, AppError> {
        let Some(from) = cmd.template.take() else { return Ok(None) };
        if !cmd.actions.is_empty() || cmd.delegation != Delegation::default() {
            let why = "the template says the actions and the delegation; name neither";
            return Err(AppError::invalid("template", why));
        }
        let Scope::Object(at) = cmd.scope else {
            return Err(AppError::invalid("scope", "the object the template is applied to"));
        };
        // Applying a template at an object takes reading it, as finding the template there did,
        // so where a template is offered is never said of an object the caller cannot read.
        let read = format!("{}.read", read_namespace(at.kind));
        if !self.permitted(caller, &read, at).await? {
            return Err(AppError::NotFound { kind: at.kind.as_str() });
        }
        let template = self.store.template(from.template).await?.ok_or_else(not_found)?;
        if !template.offered_within(&self.organizations_above(at).await?) {
            return Err(not_found());
        }
        if template.current.version != from.version {
            let why = format!(
                "the template is at version {}; read it again and issue what it says now",
                template.current.version
            );
            return Err(AppError::conflict("template.changed", why));
        }
        let applied = template
            .current
            .body
            .apply(at, self.clock.now())
            .map_err(|e| AppError::invalid("scope", e.to_string()))?;
        cmd.actions = applied.actions;
        cmd.scope = applied.scope;
        cmd.delegation = applied.delegation;
        cmd.conditions = from.conditions.unwrap_or(applied.conditions);
        if cmd.label.is_empty() {
            template.current.name.as_str().clone_into(&mut cmd.label);
        }
        Ok(Some(TemplateRef { template: template.id, version: from.version }))
    }
}
