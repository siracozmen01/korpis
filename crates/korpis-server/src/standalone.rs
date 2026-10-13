// SPDX-License-Identifier: AGPL-3.0-or-later

//! A relay running as its own process (§3.1 of docs/02-architecture.md): `korpis-server relay`.
//! It joins once, with an enrollment token an operator created, and keeps in its directory what
//! it was given: its identity, its certificate under the installation's authority, and that
//! authority, pinned by the fingerprint the join command carried.

mod dir;
mod join;

pub use dir::RelayDir;
pub use join::{JoinError, join};
