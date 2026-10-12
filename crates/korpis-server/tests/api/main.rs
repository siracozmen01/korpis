// SPDX-License-Identifier: AGPL-3.0-or-later

//! The client API end to end: install into a throwaway database, serve over TLS, and call it as
//! a client would. Needs the development database (`scripts/dev-pg.sh up`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod authority;
mod authz;
mod lifecycle;
mod operator;
mod plans;
mod quota;
mod recipes;
mod support;
mod tenancy;
mod workload;
