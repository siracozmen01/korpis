// SPDX-License-Identifier: AGPL-3.0-or-later

//! Why a backup, a restore, or a verification failed.

use crate::seal::SealError;
use crate::store::StoreError;

/// Why a job failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem call failed, on the path named.
    #[error("{path}: {source}")]
    Io {
        /// Where, relative to the volume.
        path: String,
        /// What the system said.
        source: std::io::Error,
    },
    /// The object store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// An object did not open.
    #[error("{name}: {source}")]
    Seal {
        /// The object.
        name: String,
        /// Why.
        source: SealError,
    },
    /// The repository holds something the format does not allow: a missing object, an object
    /// whose identifier is not its content's, or a tree that does not decode or names an entry
    /// a restore must not create.
    #[error("the repository is corrupt: {0}")]
    Corrupt(String),
    /// What was asked cannot be done: the target is not empty, the path is not in the backup.
    #[error("{0}")]
    Refused(String),
    /// What was restored is not what was backed up.
    #[error("verification failed: {0}")]
    Mismatch(String),
}

/// The result of a job's step.
pub type Result<T> = std::result::Result<T, Error>;

/// A rustix error at `path`.
pub(crate) fn sys(path: &str) -> impl FnOnce(rustix::io::Errno) -> Error + '_ {
    move |e| Error::Io { path: path.to_owned(), source: e.into() }
}
