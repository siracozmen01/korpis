// SPDX-License-Identifier: AGPL-3.0-or-later

//! The assignment set the node holds: every assignment verified against the control plane's
//! assignment keys before it is believed, the set digest recomputed, and the set kept on disk with
//! its signatures, so the node keeps doing its job while the control plane is away and can prove
//! what it was told.

use buffa::Message as _;
use ed25519_dalek::{Signature, VerifyingKey};
use korpis_core::id::IdKind;
use korpis_core::workload::assignment::set_digest;
use korpis_proto::korpis::agent::v1 as agent;
use std::collections::BTreeMap;
use std::path::Path;
use uuid::Uuid;

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
    /// The set as received, signatures included, one assignment each in the order of
    /// `assignments`.
    pub raw: agent::AssignmentSet,
}

impl HeldSet {
    /// The ports of the node its endpoints hold, which it forwards to its workloads.
    #[must_use]
    pub fn forwarded(&self) -> Vec<u16> {
        self.assignments
            .values()
            .flat_map(|a| a.endpoints.iter())
            .filter_map(|e| e.address.as_option())
            .filter_map(|a| u16::try_from(a.port).ok())
            .collect()
    }
}

fn verifier(keys: &[agent::SigningKey], id: &str) -> Result<VerifyingKey, Refused> {
    let key = keys.iter().find(|k| k.key_id == id).ok_or_else(|| Refused::UnknownKey(id.into()))?;
    let bytes: [u8; 32] =
        key.public_key.as_slice().try_into().map_err(|_| Refused::UnknownKey(id.into()))?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| Refused::UnknownKey(id.into()))
}

/// The assignment `signed`, the `i`th of its message, if it verifies against `keys` and is for
/// `node`, with its workload.
fn verified(
    i: usize,
    signed: &agent::SignedAssignment,
    keys: &[agent::SigningKey],
    node: &str,
) -> Result<(Uuid, agent::Assignment), Refused> {
    let key = verifier(keys, &signed.key_id)?;
    let signature =
        Signature::from_slice(&signed.signature).map_err(|_| Refused::BadSignature(i))?;
    key.verify_strict(&signed.assignment, &signature).map_err(|_| Refused::BadSignature(i))?;
    let a = agent::Assignment::decode_from_slice(&signed.assignment)
        .map_err(|_| Refused::Malformed(i))?;
    if a.node != node {
        return Err(Refused::OtherNode(i));
    }
    let workload = korpis_wire::id(IdKind::Workload, "workload", &a.workload)
        .map_err(|_| Refused::Malformed(i))?;
    Ok((workload, a))
}

impl HeldSet {
    /// `set`, if every assignment in it verifies, is for `node`, and the digest is its own.
    pub fn verify(
        set: &agent::AssignmentSet,
        keys: &[agent::SigningKey],
        node: &str,
    ) -> Result<Self, Refused> {
        let mut assignments = BTreeMap::new();
        let mut signed_by = BTreeMap::new();
        let mut entries = Vec::with_capacity(set.assignments.len());
        for (i, signed) in set.assignments.iter().enumerate() {
            let (workload, a) = verified(i, signed, keys, node)?;
            entries.push((workload, a.generation));
            // One assignment a workload: a second would be held and never run.
            if signed_by.insert(a.workload.clone(), signed.clone()).is_some() {
                return Err(Refused::Malformed(i));
            }
            assignments.insert(a.workload.clone(), a);
        }
        if set_digest(&entries) != set.set_digest {
            return Err(Refused::Digest);
        }
        let raw = agent::AssignmentSet {
            assignments: signed_by.into_values().collect(),
            set_digest: set.set_digest.clone(),
            ..Default::default()
        };
        Ok(Self { assignments, digest: set.set_digest.clone(), raw })
    }

    /// This set with `delta` applied, if this is its base, every assignment it adds verifies and
    /// is for `node`, and what it makes has the digest it claims. Only what it adds is verified:
    /// the rest was, as it arrived. `None` when this is not its base, which a resync answers.
    #[must_use]
    pub fn apply(
        &self,
        delta: &agent::AssignmentDelta,
        keys: &[agent::SigningKey],
        node: &str,
    ) -> Option<Result<Self, Refused>> {
        (delta.base_digest == self.digest).then(|| self.applied(delta, keys, node))
    }

    fn applied(
        &self,
        delta: &agent::AssignmentDelta,
        keys: &[agent::SigningKey],
        node: &str,
    ) -> Result<Self, Refused> {
        let mut signed: BTreeMap<String, agent::SignedAssignment> =
            self.assignments.keys().cloned().zip(self.raw.assignments.iter().cloned()).collect();
        let mut assignments = self.assignments.clone();
        for gone in &delta.removed {
            signed.remove(gone);
            assignments.remove(gone);
        }
        for (i, s) in delta.upserts.iter().enumerate() {
            let (_, a) = verified(i, s, keys, node)?;
            signed.insert(a.workload.clone(), s.clone());
            assignments.insert(a.workload.clone(), a);
        }
        let mut entries = Vec::with_capacity(assignments.len());
        for a in assignments.values() {
            let workload = korpis_wire::id(IdKind::Workload, "workload", &a.workload)
                .map_err(|_| Refused::Digest)?;
            entries.push((workload, a.generation));
        }
        if set_digest(&entries) != delta.set_digest {
            return Err(Refused::Digest);
        }
        let raw = agent::AssignmentSet {
            assignments: signed.into_values().collect(),
            set_digest: delta.set_digest.clone(),
            ..Default::default()
        };
        Ok(Self { assignments, digest: delta.set_digest.clone(), raw })
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    const NODE: &str = "nd_test";

    fn key() -> (SigningKey, Vec<agent::SigningKey>) {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let public = agent::SigningKey {
            key_id: "k1".to_owned(),
            public_key: signing.verifying_key().to_bytes().to_vec(),
            ..Default::default()
        };
        (signing, vec![public])
    }

    fn signed(key: &SigningKey, workload: Uuid, generation: u64) -> agent::SignedAssignment {
        let a = agent::Assignment {
            workload: korpis_wire::wire_id(IdKind::Workload, workload),
            node: NODE.to_owned(),
            generation,
            ..Default::default()
        };
        let body = a.encode_to_vec();
        let signature = key.sign(&body).to_bytes().to_vec();
        agent::SignedAssignment {
            assignment: body,
            signature,
            key_id: "k1".to_owned(),
            ..Default::default()
        }
    }

    fn whole(key: &SigningKey, of: &[(Uuid, u64)]) -> agent::AssignmentSet {
        agent::AssignmentSet {
            assignments: of.iter().map(|&(w, g)| signed(key, w, g)).collect(),
            set_digest: set_digest(of),
            ..Default::default()
        }
    }

    #[test]
    fn a_delta_makes_the_set_the_whole_set_would() {
        let (signing, keys) = key();
        let (w1, w2, w3) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let held = HeldSet::verify(&whole(&signing, &[(w1, 1), (w2, 1)]), &keys, NODE).unwrap();
        let after = [(w2, 2), (w3, 1)];
        let delta = agent::AssignmentDelta {
            base_digest: held.digest.clone(),
            upserts: vec![signed(&signing, w3, 1), signed(&signing, w2, 2)],
            removed: vec![korpis_wire::wire_id(IdKind::Workload, w1)],
            set_digest: set_digest(&after),
            ..Default::default()
        };
        let made = held.apply(&delta, &keys, NODE).unwrap().unwrap();
        let sent_whole = HeldSet::verify(&whole(&signing, &after), &keys, NODE).unwrap();
        assert_eq!(made, sent_whole, "what it holds, and what it keeps and can prove");

        let elsewhere =
            agent::AssignmentDelta { base_digest: "sha256:0".to_owned(), ..delta.clone() };
        assert!(held.apply(&elsewhere, &keys, NODE).is_none(), "not its base: a resync");
        let claimed = agent::AssignmentDelta { set_digest: held.digest.clone(), ..delta.clone() };
        let refused = held.apply(&claimed, &keys, NODE).unwrap();
        assert_eq!(refused.unwrap_err(), Refused::Digest, "not the set it claims to make");
        let mut tampered = delta;
        if let Some(b) = tampered.upserts[1].assignment.last_mut() {
            *b ^= 1;
        }
        let refused = held.apply(&tampered, &keys, NODE).unwrap();
        assert_eq!(refused.unwrap_err(), Refused::BadSignature(1), "an addition is verified");
    }

    #[test]
    fn a_set_naming_a_workload_twice_is_refused() {
        let (signing, keys) = key();
        let w = Uuid::now_v7();
        let set = whole(&signing, &[(w, 1), (w, 2)]);
        assert_eq!(HeldSet::verify(&set, &keys, NODE).unwrap_err(), Refused::Malformed(1));
    }
}
