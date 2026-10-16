// SPDX-License-Identifier: AGPL-3.0-or-later

//! Authority: the grant algebra and its evaluation (docs/08-identity.md).
//!
//! There is exactly one authorization path (P6). This module is that path: [`grant`] decides
//! whether a child may be attenuated from a parent, [`evaluate`] decides whether a request is
//! permitted by a chain of grants, and [`token`] is the wire form of a grant, verified offline
//! by relays and agents. Nothing here touches a database; the caller supplies the grants and the
//! object hierarchy, so the same code runs in the gateway, the relay, and the agent.
//!
//! The action vocabulary is plain data, needed wherever an extension's document is read, so it
//! is compiled always; everything else is behind the `authz` feature.

#[cfg(feature = "authz")]
pub mod access;
pub mod action;
#[cfg(feature = "authz")]
pub mod credential;
#[cfg(feature = "authz")]
pub mod device;
#[cfg(feature = "authz")]
pub mod evaluate;
#[cfg(feature = "authz")]
pub mod grant;
#[cfg(feature = "authz")]
pub mod link;
#[cfg(feature = "authz")]
pub mod record;
#[cfg(feature = "authz")]
pub mod renewal;
/// Scopes are plain data shared with plans and placement; they live at the crate root.
pub use crate::scope;
#[cfg(feature = "authz")]
pub mod revocation;
#[cfg(feature = "authz")]
pub mod route;
#[cfg(feature = "authz")]
mod sealed;
#[cfg(feature = "authz")]
pub mod token;

pub use action::Action;
#[cfg(feature = "authz")]
pub use evaluate::{Decision, Denial, Permit, Request, evaluate};
#[cfg(feature = "authz")]
pub use grant::{
    AttenuationError, Conditions, Delegation, Grant, StreamRange, Subject, check_attenuation,
};
#[cfg(feature = "authz")]
pub use record::{GrantRecord, GrantState};
pub use scope::{Hierarchy, Kind, LabelSelector, Node, ObjectRef, Resource, Scope};
