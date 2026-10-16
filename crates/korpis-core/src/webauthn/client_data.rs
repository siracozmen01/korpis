// SPDX-License-Identifier: AGPL-3.0-or-later

//! The client data a browser collects and signs over (W3C `WebAuthn` Level 3 §5.8.1): which
//! ceremony, for which challenge, from which origin. It is JSON the browser wrote, so it is read
//! as JSON, never compared as bytes.

use super::WebAuthnError;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;

/// Which ceremony the client data must be for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ceremony {
    /// Registering a credential.
    Create,
    /// Asserting with one.
    Get,
}

impl Ceremony {
    const fn kind(self) -> &'static str {
        match self {
            Self::Create => "webauthn.create",
            Self::Get => "webauthn.get",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(default)]
    cross_origin: bool,
    #[serde(default)]
    top_origin: Option<String>,
}

/// Checks that `json` is the client data of `ceremony`, for `challenge`, from one of `origins`,
/// and not made inside another site's frame.
pub(super) fn check(
    json: &[u8],
    ceremony: Ceremony,
    challenge: &[u8],
    origins: &[String],
) -> Result<(), WebAuthnError> {
    let data: ClientData =
        serde_json::from_slice(json).map_err(|_| WebAuthnError::ClientData("not client data"))?;
    if data.kind != ceremony.kind() {
        return Err(WebAuthnError::ClientData("another ceremony"));
    }
    let presented = URL_SAFE_NO_PAD
        .decode(data.challenge.trim_end_matches('='))
        .map_err(|_| WebAuthnError::ClientData("challenge encoding"))?;
    // A plain comparison: the challenge is a single-use nonce the client already holds.
    if presented != challenge {
        return Err(WebAuthnError::ClientData("another challenge"));
    }
    if !origins.contains(&data.origin) {
        return Err(WebAuthnError::ClientData("an origin this relying party does not serve"));
    }
    if data.cross_origin || data.top_origin.is_some() {
        return Err(WebAuthnError::ClientData("made inside another site's frame"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
        let c = URL_SAFE_NO_PAD.encode(challenge);
        format!(r#"{{"type":"{kind}","challenge":"{c}","origin":"{origin}"}}"#).into_bytes()
    }

    #[test]
    fn client_data_is_checked_for_its_ceremony_challenge_origin_and_frame() {
        let origins = vec!["https://panel.example.com".to_owned()];
        let ok = json("webauthn.get", b"abc", "https://panel.example.com");
        assert_eq!(check(&ok, Ceremony::Get, b"abc", &origins), Ok(()));
        let refused = |j: &[u8], c: &[u8]| check(j, Ceremony::Get, c, &origins).is_err();
        assert!(refused(&ok, b"abd"), "another challenge");
        assert!(refused(&json("webauthn.create", b"abc", "https://panel.example.com"), b"abc"));
        assert!(refused(&json("webauthn.get", b"abc", "https://evil.example"), b"abc"));
        let framed = br#"{"type":"webauthn.get","challenge":"YWJj","origin":"https://panel.example.com","crossOrigin":true}"#;
        assert!(refused(framed, b"abc"), "inside another site's frame");
        assert!(refused(b"not json", b"abc"));
    }
}
