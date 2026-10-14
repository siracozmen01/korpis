// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backups (§5 of docs/06-storage.md): a volume read into content-defined chunks and one tree per
//! directory, each sealed under a key derived from the workload's data key and written beneath
//! the workload's prefix in an object store, then restored whole or one path at a time, and
//! verified against what was backed up. The format is `korpis.backup.v1`.
//!
//! A backup job runs this on the node with only the capabilities it needs, the one workload's
//! data key, and a credential that can write beneath that workload's prefix and nothing else
//! (§5.5 of the same document). Nothing here deletes from a store.

mod apply;
pub mod backup;
pub mod error;
mod file;
pub mod keys;
mod meta;
pub mod name;
pub mod repo;
pub mod restore;
pub mod s3;
pub mod seal;
mod sink;
pub mod store;
pub mod verify;
mod walk;

pub use apply::{ContentSource, NoContent};
pub use backup::{Options, backup};
pub use error::{Error, Result};
pub use file::Content;
pub use keys::Keys;
pub use repo::Repo;
pub use restore::{Restore, Restored, restore};
pub use verify::verify;
