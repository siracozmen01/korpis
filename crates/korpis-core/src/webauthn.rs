// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `WebAuthn` relying party (§9.6 of docs/08-identity.md, W3C `WebAuthn` Level 3 §7): verifying
//! that a browser registered a passkey for this site, and that a later assertion was signed by
//! it. Attestation is not used, so its statement is never read: the installation has no list of
//! authenticator models to trust, and asking for one would only identify the user's device. User
//! verification is required, which is what makes a passkey a complete sign-in.
//!
//! Pure functions over the bytes the browser returned: the caller holds the challenge it issued
//! and the passkey it stored, and decides what a verified ceremony means.

mod authdata;
mod client_data;
mod key;

pub use key::Algorithm;

use authdata::AuthenticatorData;
use client_data::Ceremony;
use sha2::{Digest, Sha256};

/// The longest credential identifier accepted, as the specification bounds it.
const MAX_CREDENTIAL_ID: usize = 1023;

/// The relying party: the site passkeys are bound to, and the origins it serves the panel at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelyingParty {
    /// Its identifier: the registrable domain the panel is reached under, e.g. `example.com`.
    pub id: String,
    /// The origins a ceremony may come from, e.g. `https://panel.example.com`.
    pub origins: Vec<String>,
}

/// A passkey a registration created, to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPasskey {
    /// The credential's identifier, as the authenticator chose it.
    pub credential_id: Vec<u8>,
    /// Its public key, a COSE key as the authenticator encoded it.
    pub public_key: Vec<u8>,
    /// The signature algorithm of the key.
    pub algorithm: Algorithm,
    /// The authenticator's signature counter at registration.
    pub sign_count: u32,
    /// The authenticator model's identifier, all zeros when it gives none.
    pub aaguid: [u8; 16],
    /// Whether the credential may be synced to the holder's other devices.
    pub backup_eligible: bool,
}

/// A passkey as stored, to verify an assertion with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPasskey<'a> {
    /// The COSE key registered.
    pub public_key: &'a [u8],
    /// The signature counter last seen.
    pub sign_count: u32,
}

/// Why a ceremony does not verify. Every case is a refusal; none says more than which check
/// failed, which is all a caller should tell anyone.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebAuthnError {
    /// The client data is not a ceremony of the expected type, for this challenge and origin.
    #[error("the client data does not match the ceremony: {0}")]
    ClientData(&'static str),
    /// The authenticator data is malformed, for another site, or lacks a required flag.
    #[error("the authenticator data does not verify: {0}")]
    AuthenticatorData(&'static str),
    /// The credential's public key is malformed or of an algorithm not accepted.
    #[error("the credential's key is not accepted: {0}")]
    Key(&'static str),
    /// The signature does not verify under the stored key.
    #[error("the signature does not verify")]
    Signature,
    /// The signature counter went backwards: two authenticators hold the same credential.
    #[error("the signature counter did not advance; the credential may be cloned")]
    Counter,
}

/// Verifies a registration: `client_data_json` and `attestation_object` as the browser returned
/// them, for the `challenge` this relying party issued. Answers the passkey to store.
///
/// # Errors
/// Any check of W3C `WebAuthn` Level 3 §7.1 that fails, attestation aside.
pub fn verify_registration(
    rp: &RelyingParty,
    challenge: &[u8],
    client_data_json: &[u8],
    attestation_object: &[u8],
) -> Result<NewPasskey, WebAuthnError> {
    client_data::check(client_data_json, Ceremony::Create, challenge, &rp.origins)?;
    let auth_data = authdata::from_attestation(attestation_object)?;
    let data = AuthenticatorData::parse(&auth_data)?;
    data.check(rp_id_hash(&rp.id))?;
    let attested = data.attested.ok_or(WebAuthnError::AuthenticatorData("no credential"))?;
    if attested.credential_id.is_empty() || attested.credential_id.len() > MAX_CREDENTIAL_ID {
        return Err(WebAuthnError::AuthenticatorData("credential identifier length"));
    }
    let algorithm = key::algorithm(&attested.public_key)?;
    Ok(NewPasskey {
        credential_id: attested.credential_id,
        public_key: attested.public_key,
        algorithm,
        sign_count: data.sign_count,
        aaguid: attested.aaguid,
        backup_eligible: data.backup_eligible,
    })
}

/// Verifies an assertion by `stored`: the `client_data_json`, `authenticator_data`, and
/// `signature` as the browser returned them, for the `challenge` this relying party issued.
/// Answers the signature counter to store.
///
/// # Errors
/// Any check of W3C `WebAuthn` Level 3 §7.2 that fails, a counter that did not advance included.
pub fn verify_assertion(
    rp: &RelyingParty,
    challenge: &[u8],
    stored: &StoredPasskey<'_>,
    client_data_json: &[u8],
    authenticator_data: &[u8],
    signature: &[u8],
) -> Result<u32, WebAuthnError> {
    client_data::check(client_data_json, Ceremony::Get, challenge, &rp.origins)?;
    let data = AuthenticatorData::parse(authenticator_data)?;
    data.check(rp_id_hash(&rp.id))?;
    let mut signed = authenticator_data.to_vec();
    signed.extend_from_slice(&Sha256::digest(client_data_json));
    key::verify(stored.public_key, &signed, signature)?;
    // A counter of zero on both sides is an authenticator that keeps none, as synced passkeys
    // do; otherwise it must advance, or two authenticators hold the same credential.
    let advanced = data.sign_count > stored.sign_count;
    if (data.sign_count != 0 || stored.sign_count != 0) && !advanced {
        return Err(WebAuthnError::Counter);
    }
    Ok(data.sign_count)
}

fn rp_id_hash(id: &str) -> [u8; 32] {
    Sha256::digest(id.as_bytes()).into()
}
