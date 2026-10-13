// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `korpis` binary against a real control plane, installed into a throwaway database and
//! served on a free port. Needs the development database (`scripts/dev-pg.sh up`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod call;
mod commands;
mod completion;
mod declared;
mod extensions;
mod grants;
mod sets;
mod support;
