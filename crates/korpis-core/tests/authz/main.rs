// SPDX-License-Identifier: AGPL-3.0-or-later

//! The examples of docs/08-identity.md, run against the one authorization path.
//!
//! A three-level tenancy tree: a host, a reseller under it, a customer under the reseller. The
//! customer has project "Survival" with two workloads; W1 is labelled game=mc.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod approval;
mod attenuation;
mod bindings;
mod ceiling;
mod in_force;
mod issuing;
mod members;
mod private;
mod support;
mod suspension;
mod templates;
mod tokens;
