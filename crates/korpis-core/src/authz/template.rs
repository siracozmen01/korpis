// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grant templates (§4 of docs/08-identity.md): a named, versioned request for a grant, owned by
//! an organization and offered to its subtree. A template is applied by attenuating a grant the
//! issuer already holds, with the template filling in the request; it grants nobody anything,
//! and no authorization decision reads it.

use super::action::Action;
use super::grant::Delegation;
use crate::labels::LabelSelector;
use crate::object::Kind;
use crate::principal::Principal;
use crate::tenancy::Name;
use ipnet::IpNet;
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// Where a grant from a template applies, given the object it is applied to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopePattern {
    /// The kind of object it is applied to: an organization, a project, or a workload; any of
    /// the three when not said.
    pub kind: Option<Kind>,
    /// With an organization or a project: the workloads below it carrying these labels, a
    /// selector scope (§3.4 of docs/08-identity.md).
    pub selector: Option<LabelSelector>,
}

/// A template's default conditions, relative because a template outlives any instant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateConditions {
    /// A grant from it expires this long after it is issued.
    pub lifetime: Option<Duration>,
    /// Empty means unrestricted.
    pub source_cidr: Vec<IpNet>,
    /// Whether every use needs a second factor.
    pub requires_mfa: bool,
    /// Redeemed online (§6.2 of docs/08-identity.md).
    pub max_uses: Option<u64>,
}

/// What one version of a template asks for; a version, once made, never changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateBody {
    /// What a grant from it permits; at least one.
    pub actions: BTreeSet<Action>,
    /// Where.
    pub pattern: ScopePattern,
    /// Its default conditions.
    pub conditions: TemplateConditions,
    /// What a grant from it may pass on: said, never defaulted.
    pub delegation: Delegation,
}

/// One version of a template, as it was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateVersion {
    /// 1 when the template was made, and one more at every edit.
    pub version: u64,
    /// Its name at this version.
    pub name: Name,
    /// What it asks for.
    pub body: TemplateBody,
    /// Who made this version.
    pub by: Principal,
    /// When.
    pub at: SystemTime,
}

/// A template, at its current version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantTemplate {
    /// Immutable identity.
    pub id: Uuid,
    /// The organization that owns it, to whose subtree it is offered.
    pub organization: Uuid,
    /// Its current version.
    pub current: TemplateVersion,
    /// When it was retired: offered nowhere since, and kept because grants name its versions.
    pub retired_at: Option<SystemTime>,
}

/// Why a template cannot be made as asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// No action.
    #[error("a template permits at least one action")]
    NoActions,
    /// A pattern's kind is not one a template is applied to.
    #[error("a template is applied to an organization, a project, or a workload")]
    Kind,
    /// A selector without an organization or a project to look below.
    #[error("a selector looks below an organization or a project, which the kind must name")]
    SelectorKind,
    /// Delegation without a depth: unbounded depth is a root grant's alone.
    #[error("a template that delegates says how deep, at least 1")]
    Unbounded,
    /// Delegation of an action the template does not permit.
    #[error("a template delegates only actions it permits")]
    DelegatesMore,
    /// A depth or actions on a template that does not delegate.
    #[error("a template that does not delegate names no depth and no actions to delegate")]
    Inert,
    /// A lifetime of nothing.
    #[error("a lifetime is longer than nothing")]
    Lifetime,
    /// A use cap of nothing.
    #[error("a use cap is at least 1")]
    Uses,
}

impl TemplateError {
    /// The request field it is about.
    #[must_use]
    pub fn field(&self) -> &'static str {
        match self {
            Self::NoActions => "actions",
            Self::Kind | Self::SelectorKind => "scope_pattern",
            Self::Unbounded | Self::DelegatesMore | Self::Inert => "delegation",
            Self::Lifetime => "conditions.lifetime",
            Self::Uses => "conditions.max_uses",
        }
    }
}

impl TemplateBody {
    /// Checks that it asks for a grant that could be issued from it.
    pub fn validate(&self) -> Result<(), TemplateError> {
        if self.actions.is_empty() {
            return Err(TemplateError::NoActions);
        }
        let applied = [Kind::Organization, Kind::Project, Kind::Workload];
        if self.pattern.kind.is_some_and(|k| !applied.contains(&k)) {
            return Err(TemplateError::Kind);
        }
        let below = matches!(self.pattern.kind, Some(Kind::Organization | Kind::Project));
        if self.pattern.selector.is_some() && !below {
            return Err(TemplateError::SelectorKind);
        }
        let d = &self.delegation;
        if d.allowed {
            if d.max_depth.is_none_or(|n| n == 0) {
                return Err(TemplateError::Unbounded);
            }
            if !d.actions.is_subset(&self.actions) {
                return Err(TemplateError::DelegatesMore);
            }
        } else if d.max_depth.is_some() || !d.actions.is_empty() {
            return Err(TemplateError::Inert);
        }
        if self.conditions.lifetime.is_some_and(|l| l.is_zero()) {
            return Err(TemplateError::Lifetime);
        }
        if self.conditions.max_uses == Some(0) {
            return Err(TemplateError::Uses);
        }
        Ok(())
    }
}

impl GrantTemplate {
    /// Whether it is offered at an object whose ancestry, the object itself first and each
    /// organization above it after, is `ancestry`: retired templates are offered nowhere, and a
    /// live one to its owner's subtree, the owner included.
    #[must_use]
    pub fn offered_within(&self, ancestry: &[Uuid]) -> bool {
        self.retired_at.is_none() && ancestry.contains(&self.organization)
    }
}
