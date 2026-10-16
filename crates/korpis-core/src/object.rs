// SPDX-License-Identifier: AGPL-3.0-or-later

//! References to objects by kind and identifier, shared by authority, placement, and plans.

use uuid::Uuid;

/// The kinds of object a scope, a plan step, or a placement can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// An organization.
    Organization,
    /// A project.
    Project,
    /// A workload.
    Workload,
    /// A volume.
    Volume,
    /// An endpoint.
    Endpoint,
    /// A stream.
    Stream,
    /// A node.
    Node,
    /// A backup repository.
    Repository,
    /// A domain an organization holds.
    Domain,
    /// A certificate the ingress serves.
    Certificate,
}

impl Kind {
    /// The stored and wire name.
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
            Self::Repository => "repository",
            Self::Domain => "domain",
            Self::Certificate => "certificate",
        }
    }

    /// The kind a stored name names.
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
            "repository" => Self::Repository,
            "domain" => Self::Domain,
            "certificate" => Self::Certificate,
            _ => return None,
        })
    }
}

/// One object, by kind and identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectRef {
    /// What kind of object.
    pub kind: Kind,
    /// Which one.
    pub id: Uuid,
}

impl ObjectRef {
    /// A reference to object `id` of `kind`.
    #[must_use]
    pub fn new(kind: Kind, id: Uuid) -> Self {
        Self { kind, id }
    }
}
