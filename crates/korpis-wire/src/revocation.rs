// SPDX-License-Identifier: AGPL-3.0-or-later

//! The revocation list: `korpis.agent.v1.RevocationList` (§6.3 of docs/08-identity.md). Reading
//! it checks its shape only; whether it is trusted is the signature's question, answered by
//! [`SignedList::verify`] against the token keys the verifier holds.

use crate::{Result, invalid};
use korpis_core::authz::revocation::{RevocationList, SignedList};
use korpis_core::authz::token::{key_name, parse_key_name};
use korpis_proto::korpis::agent::v1 as agent;
use uuid::Uuid;

/// The wire form of a signed list.
#[must_use]
pub fn revocation_to_pb(s: &SignedList) -> agent::RevocationList {
    agent::RevocationList {
        grants: s.list.grants.iter().map(Uuid::to_string).collect(),
        version: s.list.version,
        key_id: key_name(s.key_id),
        signature: s.signature.clone(),
        ..Default::default()
    }
}

/// A signed list from the wire, not yet verified.
pub fn revocation_from_pb(r: &agent::RevocationList) -> Result<SignedList> {
    let key_id = parse_key_name(&r.key_id)
        .ok_or_else(|| invalid("revocation_list.key_id", "expected token-<epoch>"))?;
    let grants = r
        .grants
        .iter()
        .map(|g| {
            Uuid::parse_str(g).map_err(|_| invalid("revocation_list.grants", "expected UUIDs"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(SignedList {
        list: RevocationList { version: r.version, grants },
        key_id,
        signature: r.signature.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_core::authz::token::{KeyRing, SigningKey};
    use std::time::SystemTime;

    #[test]
    fn a_list_survives_the_wire_and_still_verifies() {
        let key = SigningKey::generate(3);
        let mut ring = KeyRing::default();
        ring.insert(3, &key.public_bytes()).unwrap();
        let list = RevocationList::new(SystemTime::now(), vec![Uuid::now_v7(), Uuid::now_v7()]);
        let signed = list.clone().sign(&key);
        let pb = revocation_to_pb(&signed);
        assert_eq!(pb.key_id, "token-3");
        let back = revocation_from_pb(&pb).unwrap();
        assert_eq!(back, signed);
        assert_eq!(back.verify(&ring), Ok(list));
        let bad = agent::RevocationList { key_id: "assignment-1".into(), ..pb };
        assert!(revocation_from_pb(&bad).is_err());
    }
}
