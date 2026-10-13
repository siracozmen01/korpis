// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's data directory: signing keys and TLS material, never in the database
//! (§6 of docs/18-operations.md). It is part of what Korpis' own backup must hold, outside the
//! system it protects.
//!
//! ```text
//! <data>/keys/token-<n>.key        capability-token signing key, 32 raw bytes
//! <data>/keys/assignment-<n>.key   assignment signing key, 32 raw bytes
//! <data>/tls/ca.pem, ca.key        the installation's own certificate authority
//! <data>/tls/server.pem, server.key
//! ```

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// The control plane's data directory: signing keys and TLS material, which never enter the
/// database and are backed up separately (§6 of docs/18-operations.md).
#[derive(Debug, Clone)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// The data directory at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Its root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the signing keys are.
    #[must_use]
    pub fn keys(&self) -> PathBuf {
        self.root.join("keys")
    }

    /// Where the CA and the server certificate are.
    #[must_use]
    pub fn tls(&self) -> PathBuf {
        self.root.join("tls")
    }

    /// Creates the directory tree, readable by its owner only.
    pub fn create(&self) -> std::io::Result<()> {
        for dir in [self.root.clone(), self.keys(), self.tls()] {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        Ok(())
    }
}

/// Writes `bytes` to a new file readable by its owner only, refusing to overwrite.
pub fn write_secret(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Writes a public file (certificates), refusing to overwrite.
pub fn write_public(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Replaces a public file (certificates) atomically: written beside it, then renamed over it.
pub fn replace_public(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut beside = path.as_os_str().to_owned();
    beside.push(".new");
    let beside = Path::new(&beside);
    let _ = fs::remove_file(beside);
    write_public(beside, bytes)?;
    fs::rename(beside, path)
}
