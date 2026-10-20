// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in through an identity provider (§9.7 of docs/08-identity.md). The control plane
//! begins a sign-in by making a state and a nonce and keeping both, with the provider and the
//! PKCE challenge, for ten minutes; the person returns with the state, which is spent once, and
//! the provider asserts the code they bring. The control plane keeps only the state's digest, so
//! the store's contents begin no one's sign-in.

use super::binding::{IdentityBinding, ProviderRef};
use super::credential::EntropyError;
use crate::principal::SetName;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};
use std::time::{Duration, SystemTime};

/// How long a begun sign-in waits for its person to return.
pub const PENDING_SIGN_IN: Duration = Duration::from_secs(10 * 60);
/// The redirect URI's path at the panel: the one an operator registers at their identity
/// provider.
pub const CALLBACK_PATH: &str = "/sign-in/callback";
/// The longest code an identity provider's redirect is believed to carry, in bytes.
pub const MAX_CODE: usize = 2048;

/// A sign-in begun and not yet completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSignIn {
    /// The provider asked for the address, and to be asked to assert the code.
    pub provider: ProviderRef,
    /// The nonce its ID token must carry back.
    pub nonce: String,
    /// The PKCE challenge, `S256`, of the verifier the panel keeps.
    pub code_challenge: String,
    /// When it was begun.
    pub begun_at: SystemTime,
    /// When it stops being completed.
    pub expires_at: SystemTime,
}

/// What a sign-in is begun with besides its provider: the state and nonce, as sent, and the
/// state's digest, as kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// The state, 256 random bits, returned with the person.
    pub state: String,
    /// The state's digest, which finds the pending sign-in.
    pub state_digest: [u8; 32],
    /// The nonce, 256 random bits, asserted back inside what the person returns with.
    pub nonce: String,
}

/// 256 random bits, in base64url without padding.
fn random_text() -> Result<String, EntropyError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| EntropyError(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

impl Opened {
    /// A new state and nonce.
    ///
    /// # Errors
    /// [`EntropyError`] if the operating system supplies no randomness.
    pub fn new() -> Result<Self, EntropyError> {
        let state = random_text()?;
        Ok(Self { state_digest: state_digest(&state), state, nonce: random_text()? })
    }
}

/// The digest a state is kept and found by.
#[must_use]
pub fn state_digest(state: &str) -> [u8; 32] {
    Sha256::digest(state.as_bytes()).into()
}

/// Whether `s` is an `S256` PKCE challenge: the base64url, without padding, of a SHA-256 digest
/// (RFC 7636 §4.2), so 43 characters of its alphabet.
#[must_use]
pub fn is_challenge(s: &str) -> bool {
    s.len() == 43 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether `s` is a PKCE verifier: 43 to 128 unreserved characters (RFC 7636 §4.1).
#[must_use]
pub fn is_verifier(s: &str) -> bool {
    (43..=128).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

/// Whether `verifier` is the one `challenge` was made from (RFC 7636 §4.6): checked by the
/// control plane before the provider is asked, so a code returned to the wrong browser costs no
/// call.
#[must_use]
pub fn verifies(verifier: &str, challenge: &str) -> bool {
    is_verifier(verifier)
        && URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == challenge
}

/// Of the live bindings of the sets a sign-in named, those it confirms the person in: each whose
/// required sets it named too (Finding 117 of docs/23-walkthroughs.md).
#[must_use]
pub fn confirmed_in<'a>(
    named: &[SetName],
    bound: &'a [IdentityBinding],
) -> Vec<&'a IdentityBinding> {
    bound
        .iter()
        .filter(|b| b.retired_at.is_none())
        .filter(|b| b.terms.require_membership.iter().all(|r| named.contains(r)))
        .collect()
}

/// The redirect URI of the panel at `origin`.
#[must_use]
pub fn redirect_uri(origin: &str) -> String {
    format!("{}{CALLBACK_PATH}", origin.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_is_found_by_its_digest_and_each_is_new() {
        let (a, b) = (Opened::new().unwrap(), Opened::new().unwrap());
        assert_eq!(a.state_digest, state_digest(&a.state));
        assert_ne!(a.state, b.state);
        assert_ne!(a.state, a.nonce);
        assert_eq!(a.state.len(), 43);
    }

    #[test]
    fn a_challenge_is_a_digest_in_base64url() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "RFC 7636 B");
        assert!(is_challenge(&challenge));
        assert!(!is_challenge(&format!("{challenge}=")));
        assert!(!is_challenge(&challenge[1..]));
        assert!(!is_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw+cM"));
    }

    #[test]
    fn a_verifier_is_43_to_128_unreserved_characters() {
        assert!(is_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert!(is_verifier(&"a.~".repeat(42)));
        assert!(!is_verifier(&"a".repeat(42)));
        assert!(!is_verifier(&"a".repeat(129)));
        assert!(!is_verifier(&format!("{}/", "a".repeat(43))));
    }

    #[test]
    fn a_verifier_is_checked_against_its_challenge() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert!(verifies(verifier, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!verifies(verifier, "F9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!verifies("short", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
    }

    #[test]
    fn a_sign_in_confirms_a_set_only_with_every_set_it_requires() {
        use crate::authz::binding::{BindingOrigin, BindingState, BindingTerms};
        use crate::principal::Principal;
        let set = |name: &str| SetName { realm: "groups".into(), member_of: name.into() };
        let bound = |name: &str, requires: Vec<SetName>| IdentityBinding {
            id: uuid::Uuid::now_v7(),
            provider: ProviderRef { organization: uuid::Uuid::nil(), name: "corp".into() },
            subject: Principal::ExternalSet {
                provider: "org_x/corp".into(),
                realm: "groups".into(),
                member_of: name.into(),
            },
            origin: BindingOrigin::Operator,
            terms: BindingTerms { require_membership: requires, ..BindingTerms::default() },
            state: BindingState::Verified,
            last_verified_at: None,
            verifications: 0,
            version: 1,
            made_by: Principal::Anonymous,
            made_at: SystemTime::UNIX_EPOCH,
            retired_at: None,
        };
        let ops = bound("ops", Vec::new());
        let oncall = bound("oncall", vec![set("ops")]);
        let prod = bound("prod", vec![set("staff")]);
        let mut retired = bound("old", Vec::new());
        retired.retired_at = Some(SystemTime::UNIX_EPOCH);
        let all = [ops.clone(), oncall.clone(), prod, retired];
        let named = [set("ops"), set("oncall"), set("prod"), set("old")];
        let confirmed: Vec<_> = confirmed_in(&named, &all).into_iter().map(|b| b.id).collect();
        assert_eq!(confirmed, [ops.id, oncall.id], "prod needs staff, and old is retired");
    }

    #[test]
    fn the_redirect_uri_is_the_panels_callback() {
        assert_eq!(redirect_uri("https://k.example/"), "https://k.example/sign-in/callback");
        assert_eq!(redirect_uri("https://k.example"), "https://k.example/sign-in/callback");
    }
}
