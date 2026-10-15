// SPDX-License-Identifier: AGPL-3.0-or-later

//! Korpis's first-party backup target provider for a disk (§5.6 of docs/06-storage.md): its own
//! volume, a store that cannot scope access by itself, served as the S3 subset a job uses. For
//! each job it mints a credential scoped to one prefix and the mode asked for, lasting as asked,
//! and answers with the name of the endpoint it serves on and its own certificate, which the
//! control plane turns into the address every reader reaches it at. Its credentials live in its
//! memory alone. It lists and deletes for the control plane's prune on its own volume. It is built
//! as a third party's extension would be, on the SDK alone (§1 of docs/22-first-party.md).

pub mod config;
pub mod serve;
pub mod store;
pub mod target;
pub mod tls;
pub mod tokens;

pub use target::FsTarget;

/// `bytes` in lower-case hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
