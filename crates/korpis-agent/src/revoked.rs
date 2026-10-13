// SPDX-License-Identifier: AGPL-3.0-or-later

//! The revocation list the node holds (§6.3 of docs/08-identity.md): believed only once its
//! signature verifies against the token keys enrollment returned, kept on disk with that
//! signature so a restarted agent verifies offline against the last list it received, and handed
//! to everything on the node that verifies a capability token.
//!
//! Each list replaces the one before it whole. A list from the session is taken even if its
//! version is lower than the one held: replicas compute it independently from the same rows, so
//! a lower version from the replica the agent reconnected to is as current as that replica's
//! clock, and refusing it would keep a list missing a later revocation.

use buffa::Message as _;
use korpis_core::authz::revocation::RevocationList;
use korpis_core::authz::token::{KeyRing, parse_key_name};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_wire::revocation::revocation_from_pb;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::watch;

/// The token keys among those enrollment returned, as a verifier holds them.
#[must_use]
pub fn token_ring(keys: &[agent::SigningKey]) -> KeyRing {
    let mut ring = KeyRing::default();
    for k in keys {
        let Some(epoch) = parse_key_name(&k.key_id) else { continue };
        if let Err(e) = ring.insert(epoch, &k.public_key) {
            tracing::warn!(key = %k.key_id, error = %e, "a token key is unusable");
        }
    }
    ring
}

/// `raw`, if it is well formed and its signature verifies against `ring`.
pub fn verify(raw: &agent::RevocationList, ring: &KeyRing) -> Result<RevocationList, String> {
    let signed = revocation_from_pb(raw).map_err(|e| e.to_string())?;
    signed.verify(ring).map_err(|e| e.to_string())
}

/// The list as the node holds it, and where it keeps it.
#[derive(Debug)]
pub struct Revoked {
    ring: KeyRing,
    path: PathBuf,
    tx: watch::Sender<Arc<RevocationList>>,
}

impl Revoked {
    /// The list kept at `path`, verified again against `ring`: what is on disk is believed no
    /// more than what arrives, and a kept list that no longer verifies is no list. Also what
    /// verifiers watch.
    #[must_use]
    pub fn load(path: PathBuf, ring: KeyRing) -> (Self, watch::Receiver<Arc<RevocationList>>) {
        let kept = match read(&path, &ring) {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "the kept revocation list is unusable; holding none");
                RevocationList::default()
            }
        };
        let (tx, rx) = watch::channel(Arc::new(kept));
        (Self { ring, path, tx }, rx)
    }

    /// Believes and keeps `raw`; its version and size, or why it was refused.
    pub fn receive(&self, raw: &agent::RevocationList) -> Result<(u64, usize), String> {
        let list = verify(raw, &self.ring)?;
        // Kept before it is used, so a restart never holds an older list than one acted on.
        crate::durable::replace(&self.path, &raw.encode_to_vec()).map_err(|e| e.to_string())?;
        let shape = (list.version, list.grants.len());
        self.tx.send_replace(Arc::new(list));
        Ok(shape)
    }
}

fn read(path: &Path, ring: &KeyRing) -> Result<RevocationList, String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RevocationList::default());
        }
        Err(e) => return Err(e.to_string()),
    };
    let raw = agent::RevocationList::decode_from_slice(&bytes).map_err(|e| e.to_string())?;
    verify(&raw, ring)
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_core::authz::token::{SigningKey, key_name};
    use korpis_wire::revocation::revocation_to_pb;
    use std::time::SystemTime;
    use uuid::Uuid;

    fn enrolled(key: &SigningKey) -> Vec<agent::SigningKey> {
        vec![agent::SigningKey {
            key_id: key_name(key.key_id),
            public_key: key.public_bytes(),
            ..Default::default()
        }]
    }

    #[test]
    fn a_list_is_believed_kept_and_believed_again_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked.binpb");
        let key = SigningKey::generate(1);
        let grant = Uuid::now_v7();
        let list = RevocationList::new(SystemTime::now(), vec![grant]);
        let raw = revocation_to_pb(&list.clone().sign(&key));

        let (held, rx) = Revoked::load(path.clone(), token_ring(&enrolled(&key)));
        assert!(rx.borrow().grants.is_empty(), "nothing kept is an empty list");
        assert_eq!(held.receive(&raw), Ok((list.version, 1)));
        assert_eq!(rx.borrow().refuses(&[grant]), Some(grant));

        let (_, again) = Revoked::load(path.clone(), token_ring(&enrolled(&key)));
        assert_eq!(**again.borrow(), list, "kept across a restart");
        let stranger = SigningKey::generate(1);
        let (_, other) = Revoked::load(path, token_ring(&enrolled(&stranger)));
        assert!(other.borrow().grants.is_empty(), "a kept list is verified again");
    }

    #[test]
    fn a_forged_list_is_refused_and_the_held_one_kept() {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::generate(1);
        let (held, rx) = Revoked::load(dir.path().join("r"), token_ring(&enrolled(&key)));
        let list = RevocationList::new(SystemTime::now(), vec![Uuid::now_v7()]);
        held.receive(&revocation_to_pb(&list.clone().sign(&key))).unwrap();
        let forged = revocation_to_pb(&RevocationList::default().sign(&SigningKey::generate(1)));
        assert!(held.receive(&forged).is_err());
        assert_eq!(**rx.borrow(), list);
    }
}
