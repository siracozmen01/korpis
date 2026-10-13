// SPDX-License-Identifier: AGPL-3.0-or-later

//! The assignment set the node holds (§4.8 of docs/02-architecture.md): every assignment
//! verified against the control plane's assignment keys before it is believed, the set digest
//! recomputed, and the set kept on disk with its signatures, so the node keeps doing its job
//! while the control plane is away (§4.6) and can prove what it was told (§8 of
//! docs/03-state.md).

use buffa::Message as _;
use ed25519_dalek::{Signature, VerifyingKey};
use korpis_core::id::IdKind;
use korpis_core::workload::assignment::set_digest;
use korpis_proto::korpis::agent::v1 as agent;
use std::collections::BTreeMap;
use std::path::Path;

/// Why a set was not believed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    /// An assignment names a key the node was never given.
    #[error("assignment signed by unknown key {0:?}")]
    UnknownKey(String),
    /// A signature does not verify: a tampered set, or a forged one.
    #[error("assignment {0} does not verify")]
    BadSignature(usize),
    /// An assignment does not decode.
    #[error("assignment {0} does not decode")]
    Malformed(usize),
    /// An assignment is for another node.
    #[error("assignment {0} is for another node")]
    OtherNode(usize),
    /// The digest the set claims is not the digest of what it carries.
    #[error("the set digest does not match its assignments")]
    Digest,
}

/// A verified set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeldSet {
    /// By workload.
    pub assignments: BTreeMap<String, agent::Assignment>,
    /// Over the set; empty before the first.
    pub digest: String,
    /// The set as received, signatures included.
    pub raw: agent::AssignmentSet,
}

fn verifier(keys: &[agent::SigningKey], id: &str) -> Result<VerifyingKey, Refused> {
    let key = keys.iter().find(|k| k.key_id == id).ok_or_else(|| Refused::UnknownKey(id.into()))?;
    let bytes: [u8; 32] =
        key.public_key.as_slice().try_into().map_err(|_| Refused::UnknownKey(id.into()))?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| Refused::UnknownKey(id.into()))
}

impl HeldSet {
    /// `set`, if every assignment in it verifies, is for `node`, and the digest is its own.
    pub fn verify(
        set: &agent::AssignmentSet,
        keys: &[agent::SigningKey],
        node: &str,
    ) -> Result<Self, Refused> {
        let mut assignments = BTreeMap::new();
        let mut entries = Vec::with_capacity(set.assignments.len());
        for (i, signed) in set.assignments.iter().enumerate() {
            let key = verifier(keys, &signed.key_id)?;
            let signature =
                Signature::from_slice(&signed.signature).map_err(|_| Refused::BadSignature(i))?;
            key.verify_strict(&signed.assignment, &signature)
                .map_err(|_| Refused::BadSignature(i))?;
            let a = agent::Assignment::decode_from_slice(&signed.assignment)
                .map_err(|_| Refused::Malformed(i))?;
            if a.node != node {
                return Err(Refused::OtherNode(i));
            }
            let workload = korpis_wire::id(IdKind::Workload, "workload", &a.workload)
                .map_err(|_| Refused::Malformed(i))?;
            entries.push((workload, a.generation));
            assignments.insert(a.workload.clone(), a);
        }
        if set_digest(&entries) != set.set_digest {
            return Err(Refused::Digest);
        }
        Ok(Self { assignments, digest: set.set_digest.clone(), raw: set.clone() })
    }

    /// The set kept at `path`, verified again: what is on disk is believed no more than what
    /// arrives. Nothing kept is an empty set.
    pub fn load(path: &Path, keys: &[agent::SigningKey], node: &str) -> std::io::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let set = agent::AssignmentSet::decode_from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        Self::verify(&set, keys, node)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }

    /// Keeps the set at `path`, replacing what was there atomically and durably.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::durable::replace(path, &self.raw.encode_to_vec())
    }
}
