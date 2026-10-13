// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an extension's document declares (§3.1 of docs/16-extensions.md): the contract it
//! speaks, the actions it needs and over what, where it reaches, what it consumes and serves, and
//! the actions and UI slots it adds. Everything else in its document describes its workload, as
//! any recipe does; this is the one section that makes the recipe an extension.

mod installation;
mod validate;

pub use installation::Installation;

use crate::authz::action::Action;
use crate::event::EventFilter;

/// The prefix of every contract version: `korpis.extension.v1`, ...
pub const API_VERSION_PREFIX: &str = "korpis.extension.v";

/// The `extension` block of an extension's document, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionManifest {
    /// The contract it speaks (§6 of docs/16-extensions.md).
    pub api_version: String,
    /// Contract capabilities it cannot work without; a contract missing one refuses the install.
    pub requires: Vec<String>,
    /// What it needs granted, each over a scope, each with the reason the installer reads.
    pub needs: Vec<Need>,
    /// What it reaches; its workload's egress policy.
    pub egress: Vec<Egress>,
    /// What it consumes. Projects are not named by a document; an installation narrows them.
    pub subscribes: Vec<EventFilter>,
    /// What it serves.
    pub provides: Vec<ProvidedInterface>,
    /// The actions it adds, `ext.<name>.<action>`.
    pub actions: Vec<ActionDecl>,
    /// Its contributions to the web surface.
    pub ui_slots: Vec<SlotDecl>,
}

/// Actions an extension needs, over one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    /// The actions.
    pub actions: Vec<Action>,
    /// What they are wanted over.
    pub scope: NeedScope,
    /// Why, for the installer reading the list.
    pub reason: String,
}

/// What a need is wanted over, resolved at install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NeedScope {
    /// The organization or project it is installed into.
    Target,
    /// The organization it is installed into, even when installed into one of its projects.
    Organization,
    /// Its own workload only.
    OwnWorkload,
}

impl NeedScope {
    /// The name a document writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Target => "target",
            Self::Organization => "organization",
            Self::OwnWorkload => "self",
        }
    }

    /// The scope a document names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Target, Self::Organization, Self::OwnWorkload].into_iter().find(|v| v.as_str() == s)
    }
}

/// A destination outside the node an extension reaches.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Egress {
    /// A DNS name or an IP address.
    pub host: String,
    /// The port.
    pub port: u16,
}

/// The kinds of thing Korpis calls out to an extension for (§5 of docs/16-extensions.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderType {
    /// DNS records for endpoints.
    Dns,
    /// A place backups are written to.
    BackupTarget,
    /// An install step kind, `<publisher>/<step>`.
    Step,
    /// A health probe.
    Health,
    /// A console protocol.
    Console,
    /// An identity assertion verifier.
    Identity,
    /// A protocol router at the edge.
    EdgeRouter,
    /// A chat platform adapter.
    ChatAdapter,
}

impl ProviderType {
    /// Every type, in declaration order.
    pub const ALL: [Self; 8] = [
        Self::Dns,
        Self::BackupTarget,
        Self::Step,
        Self::Health,
        Self::Console,
        Self::Identity,
        Self::EdgeRouter,
        Self::ChatAdapter,
    ];

    /// The name a document writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::BackupTarget => "backup_target",
            Self::Step => "step",
            Self::Health => "health",
            Self::Console => "console",
            Self::Identity => "identity",
            Self::EdgeRouter => "edge_router",
            Self::ChatAdapter => "chat_adapter",
        }
    }

    /// The type a document names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// One interface an extension serves.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProvidedInterface {
    /// What kind of provider.
    pub kind: ProviderType,
    /// Unique per type within the organization it serves; a step kind is `<publisher>/<step>`.
    pub name: String,
}

/// An action an extension adds (§5 of docs/16-extensions.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDecl {
    /// The part after `ext.<extension>.`.
    pub name: String,
    /// What it does, for the surfaces that offer it.
    pub description: String,
    /// What it takes.
    pub arguments: Vec<ActionArgument>,
}

/// One argument of an extension action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionArgument {
    /// Its name.
    pub name: String,
    /// Its type.
    pub kind: ArgumentType,
    /// Whether it must be given.
    pub required: bool,
}

/// The type of an extension action's argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArgumentType {
    /// Text.
    String,
    /// A whole number.
    Int,
    /// True or false.
    Bool,
    /// A workload, authorized for the invoker.
    Workload,
    /// A project, authorized for the invoker.
    Project,
}

impl ArgumentType {
    /// The name a document writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Int => "int",
            Self::Bool => "bool",
            Self::Workload => "workload",
            Self::Project => "project",
        }
    }

    /// The type a document names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::String, Self::Int, Self::Bool, Self::Workload, Self::Project]
            .into_iter()
            .find(|v| v.as_str() == s)
    }

    /// Whether it names a resource, which is authorized for the invoker, never for the extension
    /// (§5 of docs/16-extensions.md).
    #[must_use]
    pub const fn is_resource(self) -> bool {
        matches!(self, Self::Workload | Self::Project)
    }
}

/// A contribution to a UI slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotDecl {
    /// The slot's name.
    pub slot: String,
    /// A declarative descriptor, or the URL of an isolated frame.
    pub descriptor: String,
}
