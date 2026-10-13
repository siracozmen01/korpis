// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Korpis node agent's supervisor (§6 of docs/02-architecture.md): unprivileged, it dials
//! out to the control plane, holds the node's assignment set, and makes the node match it. It
//! never touches tenant files and holds no capability; what it can cause is bounded by what the
//! drivers and the helper will do.

#![warn(missing_docs)]

pub mod durable;
pub mod held;
pub mod join;
pub mod journal;
pub mod lease;
pub mod runtime;
pub mod session;
pub mod state;
pub mod tls;
