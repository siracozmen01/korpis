// SPDX-License-Identifier: AGPL-3.0-or-later

//! The agent against a real control plane: installed into a throwaway database and served on a
//! free port. Needs the development database (`scripts/dev-pg.sh up`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod backup;
mod backup_read;
mod console;
mod console_read;
mod console_write;
mod content;
mod data;
mod exec_probe;
mod files;
mod halting;
mod health;
mod heartbeat;
mod identity;
mod ingress;
mod join;
mod journal;
mod lease;
mod metering;
mod offload;
mod reachability;
mod reconcile;
mod refused;
mod restore;
mod revocation;
mod runs;
mod session;
mod sftp;
mod support;
mod token_keys;
mod usage_now;
