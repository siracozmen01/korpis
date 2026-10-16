// SPDX-License-Identifier: AGPL-3.0-or-later

//! Authenticator data (W3C `WebAuthn` Level 3 §6.1): the site it was made for, the flags of what
//! the authenticator checked, its signature counter, and, at registration, the credential it
//! made. A binary layout, read field by field with every length checked.

use super::WebAuthnError;
use ciborium::value::Value;
use std::io::Read as _;

/// User present.
const UP: u8 = 0x01;
/// User verified.
const UV: u8 = 0x04;
/// Backup eligible.
const BE: u8 = 0x08;
/// Backed up.
const BS: u8 = 0x10;
/// Attested credential data included.
const AT: u8 = 0x40;

/// What an authenticator says of a ceremony.
pub(super) struct AuthenticatorData {
    rp_id_hash: [u8; 32],
    flags: u8,
    /// Its signature counter.
    pub(super) sign_count: u32,
    /// Whether the credential may be synced.
    pub(super) backup_eligible: bool,
    /// The credential made, at registration.
    pub(super) attested: Option<Attested>,
}

/// A credential an authenticator made.
pub(super) struct Attested {
    pub(super) aaguid: [u8; 16],
    pub(super) credential_id: Vec<u8>,
    /// The COSE key, as encoded.
    pub(super) public_key: Vec<u8>,
}

fn malformed(what: &'static str) -> WebAuthnError {
    WebAuthnError::AuthenticatorData(what)
}

impl AuthenticatorData {
    /// Reads `bytes`.
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, WebAuthnError> {
        let (rp_id_hash, rest) = bytes.split_first_chunk::<32>().ok_or_else(|| malformed("rp"))?;
        let (&[flags], rest) = rest.split_first_chunk::<1>().ok_or_else(|| malformed("flags"))?;
        let (count, rest) = rest.split_first_chunk::<4>().ok_or_else(|| malformed("counter"))?;
        let attested = if flags & AT == 0 { None } else { Some(attested(rest)?) };
        if flags & BS != 0 && flags & BE == 0 {
            return Err(malformed("backed up but not eligible"));
        }
        Ok(Self {
            rp_id_hash: *rp_id_hash,
            flags,
            sign_count: u32::from_be_bytes(*count),
            backup_eligible: flags & BE != 0,
            attested,
        })
    }

    /// Checks it was made for the site `rp_id_hash` names, with the user present and verified.
    pub(super) fn check(&self, rp_id_hash: [u8; 32]) -> Result<(), WebAuthnError> {
        if self.rp_id_hash != rp_id_hash {
            return Err(malformed("made for another site"));
        }
        if self.flags & UP == 0 {
            return Err(malformed("the user was not present"));
        }
        if self.flags & UV == 0 {
            return Err(malformed("the user was not verified"));
        }
        Ok(())
    }
}

/// The attested credential data at the start of `rest`; extensions may follow it, and are not
/// read.
fn attested(rest: &[u8]) -> Result<Attested, WebAuthnError> {
    let (aaguid, rest) = rest.split_first_chunk::<16>().ok_or_else(|| malformed("no aaguid"))?;
    let (len, rest) = rest.split_first_chunk::<2>().ok_or_else(|| malformed("no length"))?;
    let len = usize::from(u16::from_be_bytes(*len));
    let (id, rest) = rest.split_at_checked(len).ok_or_else(|| malformed("credential id"))?;
    // The key is one CBOR item; its length is what reading it consumes.
    let mut reader = rest;
    let _: Value = ciborium::de::from_reader(&mut reader).map_err(|_| malformed("key"))?;
    let used = rest.len().saturating_sub(reader.len());
    let key = rest.get(..used).ok_or_else(|| malformed("key"))?;
    Ok(Attested { aaguid: *aaguid, credential_id: id.to_vec(), public_key: key.to_vec() })
}

/// The authenticator data inside an attestation object, its statement unread.
pub(super) fn from_attestation(object: &[u8]) -> Result<Vec<u8>, WebAuthnError> {
    let mut reader = object;
    let value: Value =
        ciborium::de::from_reader(&mut reader).map_err(|_| malformed("attestation object"))?;
    let mut trailing = [0u8; 1];
    if reader.read(&mut trailing).unwrap_or(0) != 0 {
        return Err(malformed("bytes after the attestation object"));
    }
    let Value::Map(entries) = value else { return Err(malformed("attestation object")) };
    entries
        .into_iter()
        .find_map(|(k, v)| match (k, v) {
            (Value::Text(k), Value::Bytes(b)) if k == "authData" => Some(b),
            _ => None,
        })
        .ok_or_else(|| malformed("no authenticator data"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(flags: u8, count: u32) -> Vec<u8> {
        let mut d = vec![7; 32];
        d.push(flags);
        d.extend_from_slice(&count.to_be_bytes());
        d
    }

    #[test]
    fn authenticator_data_is_checked_for_its_site_and_flags() {
        let ok = AuthenticatorData::parse(&data(UP | UV, 5)).unwrap();
        assert_eq!(ok.sign_count, 5);
        assert!(ok.check([7; 32]).is_ok());
        assert!(ok.check([8; 32]).is_err(), "another site");
        let unverified = AuthenticatorData::parse(&data(UP, 0)).unwrap();
        assert!(unverified.check([7; 32]).is_err());
        assert!(AuthenticatorData::parse(&data(UP | UV | BS, 0)).is_err());
        assert!(AuthenticatorData::parse(&[0; 36]).is_err());
        assert!(AuthenticatorData::parse(&data(UP | UV | AT, 0)).is_err(), "no credential data");
    }
}
