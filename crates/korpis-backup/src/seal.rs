// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sealing an object for the store: compressed, then encrypted with XChaCha20-Poly1305 under the
//! key for its kind, with a fresh random nonce, and its kind and name bound in as associated
//! data, so an object moved to another name, or another kind's place, fails to open rather than
//! being read as what it is not. Opening bounds what it decompresses, so a hostile store cannot
//! hand a job an object that expands without limit.
//!
//! ```text
//! sealed = "KBO1" || nonce (24 bytes) || ciphertext and tag of zstd(plaintext)
//! ```

use crate::keys::{KEY_LEN, Keys};
use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::io::Read as _;

const MAGIC: &[u8; 4] = b"KBO1";
const NONCE: usize = 24;
/// The compression level: fast, and most of what a world gives up is had by it.
const LEVEL: i32 = 3;

/// The kinds of object, each under a prefix of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A run of a file's bytes.
    Chunk,
    /// One directory.
    Tree,
    /// One backup's record.
    Backup,
    /// One sealed segment of a workload's console stream, offloaded beneath its stream prefix
    /// (§4 of docs/14-streams.md).
    Segment,
}

impl Kind {
    /// Where objects of this kind are, beneath the workload's prefix.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Chunk => "chunks/",
            Self::Tree => "trees/",
            Self::Backup => "backups/",
            Self::Segment => "segments/",
        }
    }

    /// The most an object of this kind may hold, opened.
    const fn limit(self) -> u64 {
        match self {
            // The chunker's largest chunk.
            Self::Chunk => 16 << 20,
            Self::Tree | Self::Backup | Self::Segment => 256 << 20,
        }
    }

    fn key(self, keys: &Keys) -> &[u8; KEY_LEN] {
        match self {
            Self::Chunk => &keys.chunk,
            Self::Tree => &keys.tree,
            Self::Backup => &keys.backup,
            Self::Segment => &keys.segment,
        }
    }

    /// The object's name beneath the prefix: its kind's place and its identifier.
    #[must_use]
    pub fn key_of(self, id: &str) -> String {
        format!("{}{id}", self.prefix())
    }
}

/// Why an object could not be sealed or opened.
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    /// Not an object of this format.
    #[error("not a sealed object")]
    Malformed,
    /// It does not open under this key as this kind and name: altered, moved, or another
    /// workload's.
    #[error("the object does not open: altered, moved, or not this workload's")]
    Forged,
    /// It opens to more than its kind may hold.
    #[error("the object is larger than its kind allows")]
    TooLarge,
    /// Compression, or the system's randomness, failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

fn cipher(keys: &Keys, kind: Kind) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(kind.key(keys).into())
}

/// `plaintext`, sealed as the object `kind`/`id`.
pub fn seal(keys: &Keys, kind: Kind, id: &str, plaintext: &[u8]) -> Result<Vec<u8>, SealError> {
    let compressed = zstd::encode_all(plaintext, LEVEL)?;
    let mut nonce = [0u8; NONCE];
    getrandom::fill(&mut nonce).map_err(|e| std::io::Error::other(e.to_string()))?;
    let aad = kind.key_of(id);
    let payload = Payload { msg: &compressed, aad: aad.as_bytes() };
    let sealed = cipher(keys, kind)
        .encrypt(&XNonce::from(nonce), payload)
        .map_err(|_| SealError::Malformed)?;
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE + sealed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// The plaintext of the object `kind`/`id`, sealed as `sealed`.
pub fn open(keys: &Keys, kind: Kind, id: &str, sealed: &[u8]) -> Result<Vec<u8>, SealError> {
    let rest = sealed.strip_prefix(MAGIC.as_slice()).ok_or(SealError::Malformed)?;
    let (nonce, ciphertext) = rest.split_at_checked(NONCE).ok_or(SealError::Malformed)?;
    let nonce: [u8; NONCE] = nonce.try_into().map_err(|_| SealError::Malformed)?;
    let aad = kind.key_of(id);
    let payload = Payload { msg: ciphertext, aad: aad.as_bytes() };
    let compressed =
        cipher(keys, kind).decrypt(&XNonce::from(nonce), payload).map_err(|_| SealError::Forged)?;
    let mut plain = Vec::new();
    let decoder = zstd::stream::read::Decoder::new(compressed.as_slice())?;
    decoder.take(kind.limit() + 1).read_to_end(&mut plain)?;
    if u64::try_from(plain.len()).unwrap_or(u64::MAX) > kind.limit() {
        return Err(SealError::TooLarge);
    }
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_opens_only_as_itself() {
        let keys = Keys::derive(&[7; KEY_LEN]);
        let sealed = seal(&keys, Kind::Chunk, "abc", b"the overworld").unwrap();
        assert_eq!(open(&keys, Kind::Chunk, "abc", &sealed).unwrap(), b"the overworld");
        assert!(matches!(open(&keys, Kind::Chunk, "abd", &sealed), Err(SealError::Forged)));
        assert!(matches!(open(&keys, Kind::Tree, "abc", &sealed), Err(SealError::Forged)));
        let other = Keys::derive(&[8; KEY_LEN]);
        assert!(matches!(open(&other, Kind::Chunk, "abc", &sealed), Err(SealError::Forged)));
        let mut flipped = sealed.clone();
        if let Some(b) = flipped.last_mut() {
            *b ^= 1;
        }
        assert!(matches!(open(&keys, Kind::Chunk, "abc", &flipped), Err(SealError::Forged)));
        assert!(matches!(open(&keys, Kind::Chunk, "abc", b"KBO"), Err(SealError::Malformed)));
        assert_ne!(sealed, seal(&keys, Kind::Chunk, "abc", b"the overworld").unwrap(), "nonces");
    }
}
