// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's two signing keys, separate because they have different jobs and different
//! fates on restore (finding 16 of docs/23-walkthroughs.md): the token key signs capability tokens
//! and is rotated on every restore; the assignment key signs what nodes are told and is kept, so
//! a restored control plane can verify the assignments nodes present (§8 of docs/03-state.md).

use crate::data::{DataDir, write_secret};
use ed25519_dalek::SigningKey as AssignmentKey;
use korpis_core::authz::token::SigningKey as TokenKey;
use std::path::PathBuf;
use std::sync::Arc;

/// Why the keys could not be loaded or created.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    /// Reading or writing a key file failed.
    #[error("{path}: {source}")]
    Io {
        /// The key file.
        path: PathBuf,
        /// What failed.
        source: std::io::Error,
    },
    /// A key file does not hold a key.
    #[error("{0}: not a 32-byte key")]
    Malformed(PathBuf),
    /// The operating system could not supply randomness.
    #[error("randomness: {0}")]
    Random(String),
    /// The token key bytes were refused.
    #[error("token key: {0}")]
    Token(String),
}

/// The keys the control plane signs with.
#[derive(Debug)]
pub struct Keys {
    /// Signs capability tokens.
    pub token: Arc<TokenKey>,
    /// Signs assignments.
    pub assignment: AssignmentKey,
    /// The assignment key's identifier.
    pub assignment_key_id: String,
}

/// The first epoch of each key. The token key's epoch rises each time a restore rotates it (§8
/// of docs/03-state.md); the assignment key's never does, since nodes present what it signed.
pub const FIRST_EPOCH: u32 = 1;

/// The token key's current epoch: the highest `token-<n>.key` in the data directory, or the
/// first when there is none yet.
///
/// # Errors
/// The directory could not be read.
pub fn token_epoch(data: &DataDir) -> Result<u32, KeyError> {
    let dir = data.keys();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FIRST_EPOCH),
        Err(source) => return Err(KeyError::Io { path: dir, source }),
    };
    let epochs = entries.filter_map(Result::ok).filter_map(|e| {
        let name = e.file_name();
        name.to_str()?.strip_prefix("token-")?.strip_suffix(".key")?.parse::<u32>().ok()
    });
    Ok(epochs.max().unwrap_or(FIRST_EPOCH))
}

/// Makes the token key of the next epoch, which every later load signs with; its epoch. The keys
/// before it stay in the data directory, and the store retires them.
///
/// # Errors
/// The directory could not be read, or the key written.
pub fn rotate_token(data: &DataDir) -> Result<u32, KeyError> {
    let next = token_epoch(data)?
        .checked_add(1)
        .ok_or_else(|| KeyError::Token("the token key's epochs are exhausted".into()))?;
    let path = data.keys().join(format!("token-{next}.key"));
    write_secret(&path, &random32()?).map_err(|source| KeyError::Io { path, source })?;
    Ok(next)
}

fn random32() -> Result<[u8; 32], KeyError> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| KeyError::Random(e.to_string()))?;
    Ok(b)
}

/// The 32 secret bytes at `path`, creating them, readable by the owner alone, if there are none.
pub(crate) fn load_or_create(path: PathBuf) -> Result<[u8; 32], KeyError> {
    match std::fs::read(&path) {
        Ok(bytes) => bytes.try_into().map_err(|_| KeyError::Malformed(path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = random32()?;
            write_secret(&path, &key).map_err(|source| KeyError::Io { path, source })?;
            Ok(key)
        }
        Err(source) => Err(KeyError::Io { path, source }),
    }
}

impl Keys {
    /// Loads the keys of the current epochs, generating any that do not exist yet.
    pub fn load_or_create(data: &DataDir) -> Result<Self, KeyError> {
        let epoch = token_epoch(data)?;
        let token_bytes = load_or_create(data.keys().join(format!("token-{epoch}.key")))?;
        let token = TokenKey::from_bytes(epoch, &token_bytes)
            .map_err(|e| KeyError::Token(e.to_string()))?;
        let assignment_bytes =
            load_or_create(data.keys().join(format!("assignment-{FIRST_EPOCH}.key")))?;
        Ok(Self {
            token: Arc::new(token),
            assignment: AssignmentKey::from_bytes(&assignment_bytes),
            assignment_key_id: format!("assignment-{FIRST_EPOCH}"),
        })
    }

    /// The token key's identifier.
    #[must_use]
    pub fn token_key_id(&self) -> String {
        korpis_core::authz::token::key_name(self.token.key_id)
    }

    /// The public halves, to register with the store.
    #[must_use]
    pub fn public(&self) -> Vec<korpis_app::install::PublicKey> {
        vec![
            korpis_app::install::PublicKey {
                key_id: self.token_key_id(),
                purpose: "token",
                public: self.token.public_bytes(),
            },
            korpis_app::install::PublicKey {
                key_id: self.assignment_key_id.clone(),
                purpose: "assignment",
                public: self.assignment.verifying_key().as_bytes().to_vec(),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_token_key_is_the_one_signed_with_and_rotation_makes_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path().join("data"));
        data.create().unwrap();
        assert_eq!(token_epoch(&data).unwrap(), FIRST_EPOCH);
        let first = Keys::load_or_create(&data).unwrap();
        assert_eq!(first.token_key_id(), korpis_core::authz::token::key_name(1));
        assert_eq!(rotate_token(&data).unwrap(), 2);
        let second = Keys::load_or_create(&data).unwrap();
        assert_eq!(second.token.key_id, 2);
        assert_ne!(second.token.public_bytes(), first.token.public_bytes());
        assert_eq!(second.assignment_key_id, first.assignment_key_id, "never rotated");
        assert!(data.keys().join("token-1.key").exists(), "kept, and retired in the store");
    }
}
