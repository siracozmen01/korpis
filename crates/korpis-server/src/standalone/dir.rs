// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a relay keeps: its identity key and TLS key, readable by the relay alone; its
//! certificate chain and the installation's authority; the control plane it joined and the URL
//! it serves; and, written last, its identifier, so a join that stopped half way is not taken
//! for one that finished.
//!
//! The TLS key, chain, and expiry come in generations: the join writes the first, each renewal
//! writes the next in full, and only then points `tls.current` at it, so a renewal that stops
//! half way leaves the relay serving the generation before.

use crate::data::{replace_public, write_secret};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

/// One generation of a relay's TLS material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFiles {
    /// The private key, PEM, readable by the relay alone.
    pub key: PathBuf,
    /// The certificate, then the authority's, PEM.
    pub chain: PathBuf,
    /// When the certificate expires, in seconds since the Unix epoch.
    pub expires: PathBuf,
}

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

    /// The relay's identity key, once it has one.
    pub fn identity(&self) -> std::io::Result<ed25519_dalek::SigningKey> {
        let seed = std::fs::read(self.file("identity.key"))?;
        let seed: [u8; 32] =
            seed.try_into().map_err(|_| std::io::Error::other("identity.key is not 32 bytes"))?;
        Ok(ed25519_dalek::SigningKey::from_bytes(&seed))
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

    /// Generation `n` of the TLS material: the join's is the first.
    #[must_use]
    pub fn tls_files(&self, n: u64) -> TlsFiles {
        let name = |ext: &str| {
            if n == 0 { format!("tls.{ext}") } else { format!("tls.{n}.{ext}") }
        };
        TlsFiles {
            key: self.file(&name("key")),
            chain: self.file(&name("pem")),
            expires: self.file(&name("expires")),
        }
    }

    /// The generation served.
    pub fn tls_generation(&self) -> std::io::Result<u64> {
        match read_text(&self.file("tls.current"))? {
            None => Ok(0),
            Some(n) => n.parse().map_err(|_| std::io::Error::other("tls.current is not a number")),
        }
    }

    /// Serves generation `n` from now on, and forgets the ones before it.
    pub fn switch_tls(&self, n: u64) -> std::io::Result<()> {
        let before = self.tls_generation()?;
        replace_public(&self.file("tls.current"), n.to_string().as_bytes())?;
        for old in before..n {
            let files = self.tls_files(old);
            [files.key, files.chain, files.expires].iter().try_for_each(|p| forget(p))?;
        }
        Ok(())
    }

    /// When the certificate of `files` expires, if that was recorded.
    pub fn expires(&self, files: &TlsFiles) -> std::io::Result<Option<std::time::SystemTime>> {
        let Some(text) = read_text(&files.expires)? else { return Ok(None) };
        let seconds: u64 =
            text.parse().map_err(|_| std::io::Error::other("an expiry is not a number"))?;
        Ok(std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds)))
    }

    /// Writes a secret file of a generation not yet served, replacing what an attempt that
    /// stopped half way left there.
    pub fn secret(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        forget(path)?;
        write_secret(path, bytes)
    }

    /// Writes a public file, replacing what was there.
    pub fn public(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        replace_public(&self.file(name), bytes)
    }

    /// Writes the public file at `path`, one of this directory's, replacing what was there.
    pub fn public_at(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        replace_public(path, bytes)
    }
}

/// Removes `path`, if it is there.
fn forget(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn read_text(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.trim().to_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
