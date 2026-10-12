// SPDX-License-Identifier: AGPL-3.0-or-later

//! Scopes and containment (§3.3 and §3.4 of docs/08-identity.md).
//!
//! ```text
//! volume | endpoint | stream ⊑ their workload ⊑ its project ⊑ its organization
//! organization ⊑ any ancestor organization;  Selector(P, expr) ⊑ P
//! ```
//!
//! Containment needs the object hierarchy, which lives in the store. The caller supplies it as a
//! [`Hierarchy`], so evaluation stays pure.

use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Organization,
    Project,
    Workload,
    Volume,
    Endpoint,
    Stream,
    Node,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Organization => "organization",
            Self::Project => "project",
            Self::Workload => "workload",
            Self::Volume => "volume",
            Self::Endpoint => "endpoint",
            Self::Stream => "stream",
            Self::Node => "node",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "organization" => Self::Organization,
            "project" => Self::Project,
            "workload" => Self::Workload,
            "volume" => Self::Volume,
            "endpoint" => Self::Endpoint,
            "stream" => Self::Stream,
            "node" => Self::Node,
            _ => return None,
        })
    }
}

/// One object, by kind and identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectRef {
    pub kind: Kind,
    pub id: Uuid,
}

impl ObjectRef {
    #[must_use]
    pub fn new(kind: Kind, id: Uuid) -> Self {
        Self { kind, id }
    }
}

/// A label selector: every `match_labels` pair must be present, and every expression must hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelSelector {
    pub match_labels: BTreeMap<String, String>,
    pub expressions: Vec<LabelExpression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelExpression {
    In(String, Vec<String>),
    NotIn(String, Vec<String>),
    Exists(String),
    DoesNotExist(String),
}

impl LabelSelector {
    #[must_use]
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.match_labels.iter().all(|(k, v)| labels.get(k) == Some(v))
            && self.expressions.iter().all(|e| match e {
                LabelExpression::In(k, vs) => labels.get(k).is_some_and(|v| vs.contains(v)),
                LabelExpression::NotIn(k, vs) => labels.get(k).is_none_or(|v| !vs.contains(v)),
                LabelExpression::Exists(k) => labels.contains_key(k),
                LabelExpression::DoesNotExist(k) => !labels.contains_key(k),
            })
    }

    /// Whether every object this selector matches is also matched by `wider`: it carries all of
    /// `wider`'s requirements, and possibly more. Conservative: a narrower selector written
    /// differently is refused rather than reasoned about.
    #[must_use]
    pub fn is_at_least_as_narrow_as(&self, wider: &Self) -> bool {
        wider.match_labels.iter().all(|(k, v)| self.match_labels.get(k) == Some(v))
            && wider.expressions.iter().all(|e| self.expressions.contains(e))
    }
}

/// Where a grant applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Object(ObjectRef),
    /// Every object inside `within` that the selector matches at evaluation time.
    Selector {
        within: ObjectRef,
        selector: LabelSelector,
    },
}

impl Scope {
    /// The object the scope is anchored at.
    #[must_use]
    pub fn anchor(&self) -> ObjectRef {
        match self {
            Self::Object(o) | Self::Selector { within: o, .. } => *o,
        }
    }
}

/// One link of a resource's ancestry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub object: ObjectRef,
    pub labels: BTreeMap<String, String>,
}

/// An object together with its ancestry, from the object itself up to its root organization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub chain: Vec<Node>,
}

impl Resource {
    #[must_use]
    pub fn object(&self) -> Option<ObjectRef> {
        self.chain.first().map(|n| n.object)
    }

    /// The organization that owns the object: the nearest organization in its chain.
    #[must_use]
    pub fn owning_organization(&self) -> Option<Uuid> {
        self.chain.iter().find(|n| n.object.kind == Kind::Organization).map(|n| n.object.id)
    }

    fn position(&self, object: ObjectRef) -> Option<usize> {
        self.chain.iter().position(|n| n.object == object)
    }

    /// Whether `scope` contains this resource.
    #[must_use]
    pub fn is_within(&self, scope: &Scope) -> bool {
        match scope {
            Scope::Object(o) => self.position(*o).is_some(),
            Scope::Selector { within, selector } => self
                .position(*within)
                .is_some_and(|at| self.chain[..at].iter().any(|n| selector.matches(&n.labels))),
        }
    }
}

/// The object hierarchy, as the store knows it now.
pub trait Hierarchy {
    /// The ancestry of `object`, starting with the object itself, or `None` if it does not
    /// exist.
    fn resource(&self, object: ObjectRef) -> Option<Resource>;
}

/// Whether `child` is contained in `parent` (`child ⊑ parent`).
pub fn scope_within(child: &Scope, parent: &Scope, hierarchy: &impl Hierarchy) -> bool {
    let Some(anchor) = hierarchy.resource(child.anchor()) else {
        return false;
    };
    match (child, parent) {
        (Scope::Object(_), _) => anchor.is_within(parent),
        (Scope::Selector { .. }, Scope::Object(p)) => anchor.position(*p).is_some(),
        (
            Scope::Selector { selector: narrow, .. },
            Scope::Selector { within: p, selector: wide },
        ) => anchor.position(*p).is_some() && narrow.is_at_least_as_narrow_as(wide),
    }
}
