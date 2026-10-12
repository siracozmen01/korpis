// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Korpis domain: the nouns of docs/01-model.md and the rules between them.
//!
//! Pure by rule (§1 of ARCHITECTURE.md): no I/O, no async runtime, no database, no wire types.
//! The same code runs in the control plane, the agent, and the relay.

#![warn(missing_docs)]

pub mod audit;
#[cfg(feature = "authz")]
pub mod authz;
pub mod build;
pub mod id;
pub mod labels;
pub mod node;
pub mod object;
pub mod placement;
pub mod principal;
pub mod quantity;
pub mod recipe;
pub mod schedule;
pub mod scope;
pub mod tenancy;
pub mod value;
pub mod workload;
