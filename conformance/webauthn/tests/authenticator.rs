// SPDX-License-Identifier: AGPL-3.0-or-later

//! The relying party against a software authenticator (§9.6 of docs/08-identity.md): a passkey
//! registered and asserted with as a browser would, and every way a ceremony is refused.

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use korpis_core::webauthn::{
    Algorithm, NewPasskey, RelyingParty, StoredPasskey, WebAuthnError, verify_assertion,
    verify_registration,
};
use passkey::authenticator::{Authenticator, UiHint, UserCheck, UserValidationMethod};
use passkey::client::{Client, DefaultClientData};
use passkey::crypto::AvailableBackend;
use passkey::types::Passkey;
use passkey::types::ctap2::{Aaguid, Ctap2Error};
use passkey::types::webauthn::{
    AttestationConveyancePreference, AuthenticatedPublicKeyCredential,
    AuthenticatorSelectionCriteria, CredentialCreationOptions, CredentialRequestOptions,
    PublicKeyCredentialCreationOptions, PublicKeyCredentialParameters,
    PublicKeyCredentialRequestOptions, PublicKeyCredentialRpEntity, PublicKeyCredentialType,
    PublicKeyCredentialUserEntity, ResidentKeyRequirement, UserVerificationRequirement,
};
use url::Url;

/// A user who is always there and always verified.
struct Present;

#[async_trait::async_trait]
impl UserValidationMethod for Present {
    type PasskeyItem = Passkey;

    async fn check_user<'a>(
        &self,
        _hint: UiHint<'a, Passkey>,
        presence: bool,
        verification: bool,
    ) -> Result<UserCheck, Ctap2Error> {
        Ok(UserCheck { presence, verification })
    }

    fn is_presence_enabled(&self) -> bool {
        true
    }

    fn is_verification_enabled(&self) -> Option<bool> {
        Some(true)
    }
}

type Browser =
    Client<Option<Passkey>, Present, AvailableBackend, public_suffix::PublicSuffixList, ()>;

fn rp() -> RelyingParty {
    RelyingParty { id: "example.com".into(), origins: vec!["https://panel.example.com".into()] }
}

fn origin() -> Url {
    Url::parse("https://panel.example.com").unwrap()
}

fn browser() -> Browser {
    Client::new(Authenticator::new(Aaguid::new_empty(), None, Present, AvailableBackend))
}

/// Registers a passkey for `rp` with `challenge`, as the browser returns it.
async fn register(browser: &mut Browser, challenge: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let request = CredentialCreationOptions {
        public_key: PublicKeyCredentialCreationOptions {
            rp: PublicKeyCredentialRpEntity {
                id: Some("example.com".into()),
                name: "Korpis".into(),
            },
            user: PublicKeyCredentialUserEntity {
                id: vec![1; 16].into(),
                display_name: "friend".into(),
                name: "friend".into(),
            },
            challenge: challenge.to_vec().into(),
            pub_key_cred_params: vec![PublicKeyCredentialParameters {
                ty: PublicKeyCredentialType::PublicKey,
                alg: coset::iana::Algorithm::ES256,
            }],
            timeout: None,
            exclude_credentials: None,
            authenticator_selection: Some(AuthenticatorSelectionCriteria {
                authenticator_attachment: None,
                resident_key: Some(ResidentKeyRequirement::Required),
                require_resident_key: true,
                user_verification: UserVerificationRequirement::Required,
            }),
            hints: None,
            attestation: AttestationConveyancePreference::None,
            attestation_formats: None,
            extensions: None,
        },
    };
    let created = browser.register(&origin(), request, DefaultClientData).await.unwrap();
    let r = created.response;
    (r.client_data_json.to_vec(), r.attestation_object.to_vec())
}

async fn assert_with(browser: &mut Browser, challenge: &[u8]) -> AuthenticatedPublicKeyCredential {
    let request = CredentialRequestOptions {
        public_key: PublicKeyCredentialRequestOptions {
            challenge: challenge.to_vec().into(),
            timeout: None,
            rp_id: Some("example.com".into()),
            allow_credentials: None,
            user_verification: UserVerificationRequirement::Required,
            hints: None,
            attestation: AttestationConveyancePreference::None,
            attestation_formats: None,
            extensions: None,
        },
    };
    browser.authenticate(&origin(), request, DefaultClientData).await.unwrap()
}

fn verify(
    passkey: &NewPasskey,
    challenge: &[u8],
    a: &AuthenticatedPublicKeyCredential,
) -> Result<u32, WebAuthnError> {
    let stored = StoredPasskey { public_key: &passkey.public_key, sign_count: passkey.sign_count };
    let r = &a.response;
    verify_assertion(
        &rp(),
        challenge,
        &stored,
        &r.client_data_json,
        &r.authenticator_data,
        &r.signature,
    )
}

#[tokio::test]
async fn a_passkey_registers_and_then_signs_in() {
    let mut browser = browser();
    let (client_data, attestation) = register(&mut browser, b"registration-challenge").await;
    let passkey =
        verify_registration(&rp(), b"registration-challenge", &client_data, &attestation).unwrap();
    assert_eq!(passkey.algorithm, Algorithm::Es256);
    assert!(!passkey.credential_id.is_empty());

    let assertion = assert_with(&mut browser, b"sign-in-challenge").await;
    verify(&passkey, b"sign-in-challenge", &assertion).expect("the passkey signs in");
}

#[tokio::test]
async fn a_ceremony_for_another_challenge_site_or_key_is_refused() {
    let mut browser = browser();
    let (client_data, attestation) = register(&mut browser, b"one").await;
    let refused = verify_registration(&rp(), b"two", &client_data, &attestation);
    assert!(matches!(refused, Err(WebAuthnError::ClientData(_))), "another challenge");
    let elsewhere = RelyingParty { id: "other.com".into(), ..rp() };
    let refused = verify_registration(&elsewhere, b"one", &client_data, &attestation);
    assert!(matches!(refused, Err(WebAuthnError::AuthenticatorData(_))), "another site");
    let passkey = verify_registration(&rp(), b"one", &client_data, &attestation).unwrap();

    let assertion = assert_with(&mut browser, b"three").await;
    assert!(verify(&passkey, b"four", &assertion).is_err(), "another challenge");
    verify(&passkey, b"three", &assertion).expect("the right challenge verifies");
    let mut other = browser_with_another_key().await;
    let theirs = assert_with(&mut other, b"three").await;
    assert_eq!(verify(&passkey, b"three", &theirs), Err(WebAuthnError::Signature));
    let mut tampered = assertion;
    let mut signature = tampered.response.signature.to_vec();
    if let Some(last) = signature.last_mut() {
        *last ^= 1;
    }
    tampered.response.signature = signature.into();
    assert!(verify(&passkey, b"three", &tampered).is_err(), "a signature altered");
}

/// Another authenticator, holding a passkey of its own for the same site.
async fn browser_with_another_key() -> Browser {
    let mut other = browser();
    register(&mut other, b"theirs").await;
    other
}

#[tokio::test]
async fn a_counter_that_went_backwards_is_refused() {
    let mut browser = browser();
    let (client_data, attestation) = register(&mut browser, b"one").await;
    let passkey = verify_registration(&rp(), b"one", &client_data, &attestation).unwrap();
    let assertion = assert_with(&mut browser, b"two").await;
    let ahead = StoredPasskey { public_key: &passkey.public_key, sign_count: u32::MAX };
    let r = &assertion.response;
    let refused = verify_assertion(
        &rp(),
        b"two",
        &ahead,
        &r.client_data_json,
        &r.authenticator_data,
        &r.signature,
    );
    assert_eq!(refused, Err(WebAuthnError::Counter), "a clone's counter lags the original's");
}
