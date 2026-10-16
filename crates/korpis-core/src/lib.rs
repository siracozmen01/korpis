// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Korpis domain: the nouns of docs/01-model.md and the rules between them.
//!
//! Pure by rule (§1 of ARCHITECTURE.md): no I/O, no async runtime, no database, no wire types.
//! The same code runs in the control plane, the agent, and the relay.

#![warn(missing_docs)]

pub mod audit;
pub mod authz;
pub mod backup;
pub mod build;
#[cfg(feature = "content")]
pub mod content;
pub mod dns;
pub mod event;
pub mod extension;
pub mod id;
pub mod labels;
pub mod node;
pub mod object;
pub mod placement;
pub mod principal;
pub mod quantity;
pub mod rate;
pub mod recipe;
pub mod relay;
pub mod schedule;
pub mod scope;
pub mod tenancy;
pub mod value;
#[cfg(feature = "accounts")]
pub mod webauthn;
pub mod workload;
