// SPDX-License-Identifier: AGPL-3.0-or-later

//! SFTP at a relay an operator turns it on for (§3.1 of docs/02-architecture.md, §3.2 of
//! docs/06-storage.md). An SSH server whose one subsystem is SFTP version 3: the SSH user names
//! the workload, `wl_…`, the client proves it holds an SSH key as SSH proves it (RFC 4252 §7),
//! and the relay asks the control plane for a file token on that workload for whoever the key is
//! registered to (§9.9 of docs/08-identity.md). With it the relay opens a file session at itself,
//! from the client's address, as a client of `RelayService` would, and answers each SFTP request
//! with the session's verbs. Nothing else of SSH is served: no shell, no command, no forwarding.

mod advertised;
mod answers;
mod files;
pub mod host_key;
mod node;
mod ops;
mod ssh;

pub use advertised::Advertised;
pub use ssh::serve;

use crate::standalone::feed::FilesAsked;
use std::future::Future;
use std::time::SystemTime;

/// A file token for an SFTP client, as the control plane minted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Granted {
    /// The token.
    pub token: String,
    /// When it stops carrying the session unless renewed.
    pub expires_at: SystemTime,
    /// The route the session is opened by.
    pub route: String,
    /// `workload.file.write`, or `workload.file.read` where reading alone is allowed.
    pub action: String,
}

/// What asks the control plane for an SFTP client's file tokens: the control plane itself for its
/// own relay role, or a relay of its own over its identity key.
pub trait Authorizer: Send + Sync + 'static {
    /// A token for what `asked` names; why not, where there is none.
    fn authorize(&self, asked: FilesAsked) -> impl Future<Output = Result<Granted, String>> + Send;
}
