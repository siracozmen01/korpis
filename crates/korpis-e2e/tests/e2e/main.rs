// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node end to end: the control plane (`scripts/dev-pg.sh up`), the development registry
//! (`scripts/dev-registry.sh up`), and this machine's containerd. The node tests need root and
//! containerd, and are skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod backups;
mod console;
mod node;
mod oci;
mod recipes;
mod snapshots;
mod support;
