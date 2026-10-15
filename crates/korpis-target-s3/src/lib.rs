// SPDX-License-Identifier: AGPL-3.0-or-later

//! Korpis's first-party backup target provider for S3-compatible stores (§5.6 of
//! docs/06-storage.md). It holds the store's own credential and never hands it out: each job's
//! access is a temporary credential from the store's token service, its session policy allowing
//! one prefix and the mode asked for, and lasting no longer than asked. It lists and deletes for
//! the control plane's prune with its own credential. It is built as a third party's extension
//! would be, on the SDK alone (§1 of docs/22-first-party.md).

pub mod config;
pub mod policy;
pub mod sts;
pub mod target;

pub use target::{Place, S3Target};
