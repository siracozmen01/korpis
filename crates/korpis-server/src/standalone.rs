// SPDX-License-Identifier: AGPL-3.0-or-later

//! A relay running as its own process (§3.1 of docs/02-architecture.md): `korpis-server relay`.
//! It joins once, with an enrollment token an operator created, and keeps in its directory what
//! it was given: its identity, its certificate under the installation's authority, and that
//! authority, pinned by the fingerprint the join command carried. It then runs: it follows the
//! feed the control plane sends it, and serves the relay role by what the feed sent.

mod dir;
pub mod fed;
pub mod feed;
mod files;
mod follow;
mod join;
pub mod renew;
mod run;
pub mod served;

pub use dir::{RelayDir, TlsFiles};
pub use files::AskControl;
pub use follow::FollowError;
pub use join::{JoinError, join};
pub use run::{Edge, RunError, RunningRelay, run, url};
