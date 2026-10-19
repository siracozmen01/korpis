// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity bindings (§5 of docs/08-identity.md): how an external identity, one person or a set
//! at an identity provider, becomes a subject a grant can name. A binding grants nothing; it says
//! which provider vouches for the subject, how often that is re-checked, and, for a set, what its
//! members receive. Its provider is an identity provider extension installed into an
//! organization, and it vouches only within that organization's subtree.

use super::record::TemplateRef;
use crate::id::{Id, IdKind};
use crate::object::{Kind, ObjectRef};
use crate::principal::Principal;
use crate::scope::{Hierarchy, Scope};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How often a binding is re-verified when its declarer does not say (§5.1).
pub const DEFAULT_REVERIFY: Duration = Duration::from_secs(15 * 60);
/// The shortest re-verification interval: shorter would make the provider the bottleneck of
/// every binding it serves.
pub const MIN_REVERIFY: Duration = Duration::from_secs(60);
/// The longest re-verification interval, which bounds how long authority outlives its source.
pub const MAX_REVERIFY: Duration = Duration::from_secs(24 * 60 * 60);
/// How long an unreachable provider extends a binding when its declarer does not say (§5.1).
pub const DEFAULT_GRACE: Duration = Duration::from_secs(15 * 60);
/// The longest grace: an unreachable provider extends a membership by at most this.
pub const MAX_GRACE: Duration = Duration::from_secs(60 * 60);

/// An identity provider: an installed extension serving the `identity` type under `name` in
/// `organization`. Its key, `org_…/name`, is the provider part of every subject it vouches for,
/// so two providers never vouch for one subject, and a provider's subjects say where it may
/// vouch for them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderRef {
    /// The organization it is installed into.
    pub organization: Uuid,
    /// Its name there, unique among the organization's identity providers.
    pub name: String,
}

impl ProviderRef {
    /// The provider part of a subject key: `org_…/name`.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}/{}", Id::from_uuid(IdKind::Organization, self.organization), self.name)
    }

    /// The provider a key names.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        let (org, name) = key.split_once('/')?;
        let organization = Id::parse(IdKind::Organization, org).ok()?.uuid();
        crate::extension::slug(name).then(|| Self { organization, name: name.to_owned() })
    }

    /// The provider vouching for `subject`, if it is an external identity.
    #[must_use]
    pub fn of(subject: &Principal) -> Option<Self> {
        match subject {
            Principal::ExternalUser { provider, .. } | Principal::ExternalSet { provider, .. } => {
                Self::parse(provider)
            }
            _ => None,
        }
    }

    /// Whether it may vouch for a grant at `scope`: its organization is the scope's or one
    /// above it. A provider installed beneath, or beside, vouches for nothing there.
    #[must_use]
    pub fn reaches(&self, scope: &Scope, hierarchy: &impl Hierarchy) -> bool {
        let org = ObjectRef::new(Kind::Organization, self.organization);
        hierarchy.resource(scope.anchor()).is_some_and(|r| r.chain.iter().any(|n| n.object == org))
    }
}

/// Who made a binding (§5): sets are always declared by an operator; a person's binding is made
/// at their first authenticated use, or declared ahead of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingOrigin {
    /// Declared by someone holding `identity.binding.write` over the provider's organization.
    Operator,
    /// Made when the person first authenticated through the provider.
    FirstUse,
}

/// Whether a binding currently vouches (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingState {
    /// Its provider confirmed it within its interval and grace.
    Verified,
    /// Not confirmed, or found ended: every grant rooted in it is suspended, reversibly.
    Unverified,
}

/// One set at the same provider: a role in a guild, a group claim.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SetName {
    /// The realm it lives in: a guild, an issuer.
    pub realm: String,
    /// The set within it.
    pub member_of: String,
}

/// What a binding's declarer may change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingTerms {
    /// For a set: the template every grant to it is issued from, at this version (§3.4 of
    /// docs/08-identity.md). Without one, a grant to the set may not delegate.
    pub template: Option<TemplateRef>,
    /// For a set: other sets at the same provider its members must also be in, checked at
    /// every re-verification.
    pub require_membership: Vec<SetName>,
    /// How long an unreachable provider extends it past its interval (§5.1).
    pub grace: Duration,
    /// How often it is re-verified (§5.1).
    pub reverify_interval: Duration,
    /// For a set: the operator deliberately let its grants carry irreversible actions (§5.2).
    pub allows_irreversible: bool,
}

impl Default for BindingTerms {
    fn default() -> Self {
        Self {
            template: None,
            require_membership: Vec::new(),
            grace: DEFAULT_GRACE,
            reverify_interval: DEFAULT_REVERIFY,
            allows_irreversible: false,
        }
    }
}

/// An identity binding (§5 of docs/08-identity.md, the one definition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityBinding {
    /// Its identity.
    pub id: Uuid,
    /// The provider that verifies it.
    pub provider: ProviderRef,
    /// The external identity it binds: a person, or a set.
    pub subject: Principal,
    /// Who made it.
    pub origin: BindingOrigin,
    /// What its declarer may change.
    pub terms: BindingTerms,
    /// Whether it vouches now.
    pub state: BindingState,
    /// When its provider last confirmed it.
    pub last_verified_at: Option<SystemTime>,
    /// 1 when made, one more at every change of its terms.
    pub version: u64,
    /// Who made it.
    pub made_by: Principal,
    /// When.
    pub made_at: SystemTime,
    /// When it was retired: it vouches for nothing since, and its subject may be bound again.
    pub retired_at: Option<SystemTime>,
}

/// Why a binding is refused, naming the field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    /// The subject is not an external identity of this provider.
    #[error("the subject must be an external identity whose provider is {0}")]
    Subject(String),
    /// A set is declared by an operator, never made at first use.
    #[error("a set binding is declared by an operator")]
    SetAtFirstUse,
    /// A field that only a set binding carries was given for a person.
    #[error("only a set binding carries {0}")]
    SetOnly(&'static str),
    /// The interval is outside its bounds.
    #[error("the re-verification interval is between one minute and one day")]
    Interval,
    /// The grace is beyond its bound.
    #[error("the grace is at most one hour")]
    Grace,
    /// A required membership is malformed.
    #[error("a required set names a realm and a set, neither containing ':'")]
    Membership,
}

impl BindingError {
    /// The request field it names.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        match self {
            Self::Subject(_) => "subject",
            Self::SetAtFirstUse => "created_by",
            Self::SetOnly(field) => field,
            Self::Interval => "reverify_interval",
            Self::Grace => "constraints.grace",
            Self::Membership => "constraints.require_membership",
        }
    }
}

impl IdentityBinding {
    /// Whether it binds a set rather than a person.
    #[must_use]
    pub const fn is_set(&self) -> bool {
        matches!(self.subject, Principal::ExternalSet { .. })
    }

    /// Checks it against its own definition.
    pub fn validate(&self) -> Result<(), BindingError> {
        let key = self.provider.key();
        if ProviderRef::of(&self.subject).as_ref() != Some(&self.provider)
            || self.subject.validate().is_err()
        {
            return Err(BindingError::Subject(key));
        }
        let t = &self.terms;
        if self.is_set() {
            if self.origin == BindingOrigin::FirstUse {
                return Err(BindingError::SetAtFirstUse);
            }
        } else if t.template.is_some() {
            return Err(BindingError::SetOnly("template"));
        } else if !t.require_membership.is_empty() {
            return Err(BindingError::SetOnly("constraints.require_membership"));
        } else if t.allows_irreversible {
            return Err(BindingError::SetOnly("allows_irreversible"));
        }
        if !(MIN_REVERIFY..=MAX_REVERIFY).contains(&t.reverify_interval) {
            return Err(BindingError::Interval);
        }
        if t.grace > MAX_GRACE {
            return Err(BindingError::Grace);
        }
        let bad = |p: &str| p.is_empty() || p.contains(':');
        if t.require_membership.iter().any(|s| bad(&s.realm) || s.member_of.is_empty()) {
            return Err(BindingError::Membership);
        }
        Ok(())
    }
}
