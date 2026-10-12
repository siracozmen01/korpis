// SPDX-License-Identifier: AGPL-3.0-or-later

//! Authority: the grant algebra and its evaluation (docs/08-identity.md).
//!
//! There is exactly one authorization path (P6). This module is that path: [`grant`] decides
//! whether a child may be attenuated from a parent, [`evaluate`] decides whether a request is
//! permitted by a chain of grants, and [`token`] is the wire form of a grant, verified offline
//! by relays and agents. Nothing here touches a database; the caller supplies the grants and the
//! object hierarchy, so the same code runs in the gateway, the relay, and the agent.

pub mod action;
pub mod evaluate;
pub mod grant;
pub mod scope;
pub mod token;

pub use action::Action;
pub use evaluate::{Decision, Denial, Request, evaluate};
pub use grant::{AttenuationError, Conditions, Delegation, Grant, Subject, check_attenuation};
pub use scope::{Hierarchy, Kind, LabelSelector, Node, ObjectRef, Resource, Scope};
