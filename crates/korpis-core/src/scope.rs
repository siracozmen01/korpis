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

pub use crate::labels::{LabelExpression, LabelSelector};
pub use crate::object::{Kind, ObjectRef};

/// Where a grant applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// One object and everything below it.
    Object(ObjectRef),
    /// Every object inside `within` that the selector matches at evaluation time.
    Selector {
        /// The object the selector looks below.
        within: ObjectRef,
        /// What the objects must carry.
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
    /// The object.
    pub object: ObjectRef,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
}

/// An object together with its ancestry, from the object itself up to its root organization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    /// The object first, then its ancestors up to the root organization.
    pub chain: Vec<Node>,
}

impl Resource {
    /// The object with no ancestry: what a verifier knows that holds only the object a request
    /// names, such as a relay. A token scoped to the object itself verifies against it; one
    /// scoped above it cannot, because nothing here says what is above.
    #[must_use]
    pub fn alone(object: ObjectRef) -> Self {
        Self { chain: vec![Node { object, labels: BTreeMap::new() }] }
    }

    /// The object itself.
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
                .and_then(|at| self.chain.get(..at))
                .is_some_and(|below| below.iter().any(|n| selector.matches(&n.labels))),
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
