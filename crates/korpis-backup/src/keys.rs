// SPDX-License-Identifier: AGPL-3.0-or-later

//! The keys a backup job works under (§5.5 of docs/06-storage.md): everything is derived from the
//! one workload's data key, which the job receives for its length and nothing else. Identifiers
//! are keyed hashes under keys of their own, so equal content in two workloads has unrelated
//! identifiers and nobody can confirm another workload holds a file; each kind of object is
//! encrypted under its own key.

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

/// The length of a data key, and of every key derived from it.
pub const KEY_LEN: usize = 32;

/// What the format's keys are derived for, as HKDF's info.
const SALT: &[u8] = b"korpis.backup.v1";

/// A workload's data key and what is derived from it.
pub struct Keys {
    chunk_id: Zeroizing<[u8; KEY_LEN]>,
    tree_id: Zeroizing<[u8; KEY_LEN]>,
    digest: Zeroizing<[u8; KEY_LEN]>,
    pub(crate) chunk: Zeroizing<[u8; KEY_LEN]>,
    pub(crate) tree: Zeroizing<[u8; KEY_LEN]>,
    pub(crate) backup: Zeroizing<[u8; KEY_LEN]>,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys").finish_non_exhaustive()
    }
}

fn derive(hk: &Hkdf<Sha256>, purpose: &[u8]) -> Zeroizing<[u8; KEY_LEN]> {
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    // A 32-byte output is far below HKDF-SHA256's limit of 255 blocks, so this cannot fail.
    if hk.expand(purpose, key.as_mut()).is_err() {
        tracing::error!("HKDF refused a 32-byte key");
    }
    key
}

impl Keys {
    /// The keys derived from the data key `dek`.
    #[must_use]
    pub fn derive(dek: &[u8; KEY_LEN]) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(SALT), dek);
        Self {
            chunk_id: derive(&hk, b"chunk-id"),
            tree_id: derive(&hk, b"tree-id"),
            digest: derive(&hk, b"digest"),
            chunk: derive(&hk, b"chunk"),
            tree: derive(&hk, b"tree"),
            backup: derive(&hk, b"backup"),
        }
    }

    /// The identifier of a chunk holding `data`.
    #[must_use]
    pub fn chunk_id(&self, data: &[u8]) -> String {
        blake3::keyed_hash(&self.chunk_id, data).to_hex().to_string()
    }

    /// A hasher for a file's whole content, which a restore is verified against.
    #[must_use]
    pub fn digester(&self) -> blake3::Hasher {
        blake3::Hasher::new_keyed(&self.digest)
    }

    /// The identifier of a tree encoded as `encoded`.
    #[must_use]
    pub fn tree_id(&self, encoded: &[u8]) -> String {
        blake3::keyed_hash(&self.tree_id, encoded).to_hex().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_content_under_two_keys_has_unrelated_identifiers() {
        let (a, b) = (Keys::derive(&[1; KEY_LEN]), Keys::derive(&[2; KEY_LEN]));
        assert_eq!(a.chunk_id(b"world"), Keys::derive(&[1; KEY_LEN]).chunk_id(b"world"));
        assert_ne!(a.chunk_id(b"world"), b.chunk_id(b"world"));
        assert_ne!(a.chunk_id(b"world"), a.tree_id(b"world"), "chunks and trees apart");
        assert_eq!(a.chunk_id(b"").len(), 64);
    }
}
