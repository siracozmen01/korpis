// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a relay keeps: its identity key and TLS key, readable by the relay alone; its
//! certificate chain and the installation's authority; the control plane it joined and the URL
//! it serves; and, written last, its identifier, so a join that stopped half way is not taken
//! for one that finished.

use crate::data::{replace_public, write_secret};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

/// A relay's directory.
#[derive(Debug, Clone)]
pub struct RelayDir {
    root: PathBuf,
}

impl RelayDir {
    /// The relay kept under `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// A file in it.
    #[must_use]
    pub fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Creates it, readable by its owner alone.
    pub fn create(&self) -> std::io::Result<()> {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.root)
    }

    /// The relay's identifier, if it has joined.
    pub fn joined(&self) -> std::io::Result<Option<String>> {
        read_text(&self.file("relay"))
    }

    /// Writes a secret file once: what is there already is kept, so a join that is retried
    /// presents the same keys.
    pub fn secret_once(
        &self,
        name: &str,
        create: impl FnOnce() -> Result<Vec<u8>, String>,
    ) -> std::io::Result<Vec<u8>> {
        let path = self.file(name);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let bytes = create().map_err(std::io::Error::other)?;
                write_secret(&path, &bytes)?;
                Ok(bytes)
            }
            Err(e) => Err(e),
        }
    }

    /// Writes a public file, replacing what was there.
    pub fn public(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        replace_public(&self.file(name), bytes)
    }
}

fn read_text(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.trim().to_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
