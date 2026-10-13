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

/// The key epoch used until rotation exists: rotation is the restore procedure's, and arrives
/// with it.
pub const FIRST_EPOCH: u32 = 1;

fn random32() -> Result<[u8; 32], KeyError> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| KeyError::Random(e.to_string()))?;
    Ok(b)
}

fn load_or_create(path: PathBuf) -> Result<[u8; 32], KeyError> {
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
    /// Loads the keys of the current epoch, generating any that do not exist yet.
    pub fn load_or_create(data: &DataDir) -> Result<Self, KeyError> {
        let token_bytes = load_or_create(data.keys().join(format!("token-{FIRST_EPOCH}.key")))?;
        let token = TokenKey::from_bytes(FIRST_EPOCH, &token_bytes)
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
