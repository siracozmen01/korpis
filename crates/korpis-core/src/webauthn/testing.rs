// SPDX-License-Identifier: AGPL-3.0-or-later

//! A software authenticator, for tests of what uses the relying party: it makes an `Ed25519`
//! credential, answers registrations with attestation `none` and assertions with a counter that
//! advances, and says it verified its user, as a platform authenticator does. Its encodings are
//! written here independently of the verifier's parsing; the verifier itself is tested against
//! another implementation's authenticator in `conformance/webauthn`.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ciborium::value::Value;
use coset::{CborSerializable as _, CoseKey, KeyType, Label, RegisteredLabelWithPrivate, iana};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};

/// User present, user verified, attested credential data included.
const REGISTERED: u8 = 0x01 | 0x04 | 0x40;
/// User present, user verified.
const ASSERTED: u8 = 0x01 | 0x04;

/// A software authenticator holding one credential.
pub struct SoftAuthenticator {
    rp_id: String,
    origin: String,
    credential_id: Vec<u8>,
    key: Ed25519KeyPair,
    counter: u32,
}

impl std::fmt::Debug for SoftAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoftAuthenticator").field("rp_id", &self.rp_id).finish_non_exhaustive()
    }
}

/// What a browser returns from `navigator.credentials.create`.
#[derive(Debug, Clone)]
pub struct Created {
    /// The client data.
    pub client_data_json: Vec<u8>,
    /// The attestation object.
    pub attestation_object: Vec<u8>,
}

/// What a browser returns from `navigator.credentials.get`.
#[derive(Debug, Clone)]
pub struct Asserted {
    /// The credential that signed.
    pub credential_id: Vec<u8>,
    /// The client data.
    pub client_data_json: Vec<u8>,
    /// The authenticator data.
    pub authenticator_data: Vec<u8>,
    /// The signature.
    pub signature: Vec<u8>,
}

impl SoftAuthenticator {
    /// An authenticator for the site `rp_id`, used from `origin`, with a fresh credential.
    ///
    /// # Panics
    /// If the system has no randomness: it is for tests.
    #[must_use]
    #[expect(clippy::expect_used, reason = "a test authenticator without randomness is useless")]
    pub fn new(rp_id: &str, origin: &str) -> Self {
        let random = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&random).expect("randomness");
        let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("a key just made");
        let credential_id = crate::account::nonce().expect("randomness").to_vec();
        Self { rp_id: rp_id.into(), origin: origin.into(), credential_id, key, counter: 0 }
    }

    /// The credential's identifier.
    #[must_use]
    pub fn credential_id(&self) -> &[u8] {
        &self.credential_id
    }

    /// Sets the counter, as a cloned credential's would be.
    pub fn set_counter(&mut self, counter: u32) {
        self.counter = counter;
    }

    fn client_data(&self, kind: &str, challenge: &[u8]) -> Vec<u8> {
        serde_json::json!({
            "type": kind,
            "challenge": URL_SAFE_NO_PAD.encode(challenge),
            "origin": self.origin,
            "crossOrigin": false,
        })
        .to_string()
        .into_bytes()
    }

    fn authenticator_data(&self, flags: u8) -> Vec<u8> {
        let mut data = Sha256::digest(self.rp_id.as_bytes()).to_vec();
        data.push(flags);
        data.extend_from_slice(&self.counter.to_be_bytes());
        data
    }

    fn cose_key(&self) -> Vec<u8> {
        let key = CoseKey {
            kty: KeyType::Assigned(iana::KeyType::OKP),
            alg: Some(RegisteredLabelWithPrivate::Assigned(iana::Algorithm::EdDSA)),
            params: vec![
                (Label::Int(-1), Value::Integer(6.into())),
                (Label::Int(-2), Value::Bytes(self.key.public_key().as_ref().to_vec())),
            ],
            ..CoseKey::default()
        };
        key.to_vec().unwrap_or_default()
    }

    /// Answers a registration for `challenge`.
    #[must_use]
    pub fn create(&self, challenge: &[u8]) -> Created {
        let mut auth_data = self.authenticator_data(REGISTERED);
        auth_data.extend_from_slice(&[0; 16]);
        let len = u16::try_from(self.credential_id.len()).unwrap_or(u16::MAX);
        auth_data.extend_from_slice(&len.to_be_bytes());
        auth_data.extend_from_slice(&self.credential_id);
        auth_data.extend_from_slice(&self.cose_key());
        let object = Value::Map(vec![
            (Value::Text("fmt".into()), Value::Text("none".into())),
            (Value::Text("attStmt".into()), Value::Map(Vec::new())),
            (Value::Text("authData".into()), Value::Bytes(auth_data)),
        ]);
        let mut attestation_object = Vec::new();
        let _ = ciborium::ser::into_writer(&object, &mut attestation_object);
        Created {
            client_data_json: self.client_data("webauthn.create", challenge),
            attestation_object,
        }
    }

    /// Answers a sign-in for `challenge`, advancing the counter.
    pub fn get(&mut self, challenge: &[u8]) -> Asserted {
        self.counter = self.counter.saturating_add(1);
        let client_data_json = self.client_data("webauthn.get", challenge);
        let authenticator_data = self.authenticator_data(ASSERTED);
        let mut signed = authenticator_data.clone();
        signed.extend_from_slice(&Sha256::digest(&client_data_json));
        Asserted {
            credential_id: self.credential_id.clone(),
            client_data_json,
            authenticator_data,
            signature: self.key.sign(&signed).as_ref().to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webauthn::{
        Algorithm, RelyingParty, StoredPasskey, WebAuthnError, verify_assertion,
        verify_registration,
    };

    #[test]
    fn what_it_answers_verifies_and_a_cloned_counter_does_not() {
        let rp = RelyingParty {
            id: "example.com".into(),
            origins: vec!["https://panel.example.com".into()],
        };
        let mut a = SoftAuthenticator::new("example.com", "https://panel.example.com");
        let created = a.create(&[1; 32]);
        let new = verify_registration(
            &rp,
            &[1; 32],
            &created.client_data_json,
            &created.attestation_object,
        )
        .unwrap();
        assert_eq!(new.algorithm, Algorithm::EdDsa);
        assert_eq!(new.credential_id, a.credential_id());
        let stored = StoredPasskey { public_key: &new.public_key, sign_count: 0 };
        let got = a.get(&[2; 32]);
        let check = |g: &Asserted, s: &StoredPasskey<'_>| {
            verify_assertion(
                &rp,
                &[2; 32],
                s,
                &g.client_data_json,
                &g.authenticator_data,
                &g.signature,
            )
        };
        assert_eq!(check(&got, &stored), Ok(1));
        let seen = StoredPasskey { public_key: &new.public_key, sign_count: 5 };
        assert_eq!(check(&a.get(&[2; 32]), &seen), Err(WebAuthnError::Counter));
    }
}
