// SPDX-License-Identifier: AGPL-3.0-or-later

//! The token keys the node verifies capability tokens and revocation lists with (§6 of
//! docs/08-identity.md): those it was given when it enrolled, until the control plane sends the
//! keys in force, which it does as every session opens. A restore rotates the token key and
//! retires every earlier one (§8 of docs/03-state.md), so the set is replaced, never added to: a
//! retired key the node still held would keep the tokens it signed working. The set is kept, so a
//! restart verifies with what was last sent rather than with what enrollment gave.

use crate::revoked::token_ring;
use buffa::Message as _;
use korpis_core::authz::token::KeyRing;
use korpis_proto::korpis::agent::v1 as agent;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;

/// The keys in force, and where they are kept.
#[derive(Debug)]
pub struct TokenKeys {
    path: PathBuf,
    tx: watch::Sender<Arc<KeyRing>>,
}

impl TokenKeys {
    /// The keys kept at `path`, or `enrolled` when none are; and what verifiers watch.
    #[must_use]
    pub fn load(
        path: PathBuf,
        enrolled: &[agent::SigningKey],
    ) -> (Self, watch::Receiver<Arc<KeyRing>>) {
        let kept = std::fs::read(&path)
            .ok()
            .and_then(|bytes| agent::TokenKeys::decode_from_slice(&bytes).ok())
            .map(|k| k.keys);
        let ring = token_ring(kept.as_deref().unwrap_or(enrolled));
        let (tx, rx) = watch::channel(Arc::new(ring));
        (Self { path, tx }, rx)
    }

    /// Holds and keeps `keys` in place of every key held; how many there are, or why not.
    ///
    /// # Errors
    /// The set names no usable key, or could not be kept.
    pub fn receive(&self, keys: &agent::TokenKeys) -> Result<usize, String> {
        let ring = token_ring(&keys.keys);
        if keys.keys.is_empty() {
            return Err("a token key set with no key".into());
        }
        // Kept before it is used, so a restart never verifies with a key already retired.
        crate::durable::replace(&self.path, &keys.encode_to_vec()).map_err(|e| e.to_string())?;
        self.tx.send_replace(Arc::new(ring));
        Ok(keys.keys.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_core::authz::token::{SigningKey, key_name};

    fn set(key: &SigningKey) -> Vec<agent::SigningKey> {
        vec![agent::SigningKey {
            key_id: key_name(key.key_id),
            public_key: key.public_bytes(),
            ..Default::default()
        }]
    }

    #[test]
    fn the_keys_sent_replace_the_enrolled_ones_and_are_kept_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token_keys.binpb");
        let (old, new) = (SigningKey::generate(1), SigningKey::generate(2));
        let (held, rx) = TokenKeys::load(path.clone(), &set(&old));
        let shown = |rx: &watch::Receiver<Arc<KeyRing>>| format!("{:?}", **rx.borrow());
        let enrolled = shown(&rx);
        held.receive(&agent::TokenKeys { keys: set(&new), ..Default::default() }).unwrap();
        assert_ne!(shown(&rx), enrolled, "replaced");
        let (_, again) = TokenKeys::load(path, &set(&old));
        assert_eq!(shown(&again), shown(&rx), "what was sent, not what enrollment gave");
        assert!(held.receive(&agent::TokenKeys::default()).is_err(), "never emptied");
    }
}
