// SPDX-License-Identifier: AGPL-3.0-or-later

//! TOTP (RFC 6238 over RFC 4226): HMAC-SHA-1, six digits, thirty-second steps, one step of skew
//! either way, and each step accepted once, so a code read over someone's shoulder is spent by
//! the time it could be typed again (§9.6 of docs/08-identity.md).

use super::{EntropyError, base32, random};
use ring::hmac;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

/// How long a code lasts.
pub const STEP_SECONDS: u64 = 30;
/// How many digits a code has.
pub const DIGITS: u32 = 6;

/// A TOTP secret: 160 bits, as RFC 4226 recommends for HMAC-SHA-1.
#[derive(Clone, PartialEq, Eq)]
pub struct TotpSecret([u8; 20]);

impl std::fmt::Debug for TotpSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TotpSecret(<redacted>)")
    }
}

impl TotpSecret {
    /// A fresh secret.
    pub fn generate() -> Result<Self, EntropyError> {
        random().map(Self)
    }

    /// A secret from its 20 bytes, as the vault returns them.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Its 20 bytes, for the vault to seal.
    #[must_use]
    pub const fn bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// The `otpauth://` address an authenticator app reads, for `account` at `issuer`.
    #[must_use]
    pub fn uri(&self, issuer: &str, account: &str) -> String {
        format!(
            "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}",
            escape(issuer),
            escape(account),
            base32(&self.0),
            escape(issuer)
        )
    }

    /// The code for the thirty-second `step`.
    #[must_use]
    pub fn code(&self, step: u64) -> u32 {
        let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &self.0);
        let tag = hmac::sign(&key, &step.to_be_bytes());
        let mac = tag.as_ref();
        let offset = usize::from(mac.last().copied().unwrap_or(0) & 0x0f);
        let word = mac
            .get(offset..)
            .and_then(<[u8]>::first_chunk::<4>)
            .map_or(0, |w| u32::from_be_bytes(*w) & 0x7fff_ffff);
        word % 10u32.pow(DIGITS)
    }

    /// The step `code` is the code of at `now`, within one step either way, if it is later than
    /// `last_used`, the step of the code accepted last; the caller stores it as the new
    /// `last_used`.
    #[must_use]
    pub fn accept(&self, code: &str, now: SystemTime, last_used: Option<u64>) -> Option<u64> {
        let code: u32 = (code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()))
            .then(|| code.parse().ok())
            .flatten()?;
        let current = now.duration_since(UNIX_EPOCH).ok()?.as_secs() / STEP_SECONDS;
        [current.saturating_sub(1), current, current + 1]
            .into_iter()
            .filter(|step| last_used.is_none_or(|used| *step > used))
            .find(|step| self.code(*step) == code)
    }
}

/// `s` percent-encoded as a URI path segment or query value.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// RFC 6238 appendix B's SHA-1 secret, `12345678901234567890`.
    fn rfc() -> TotpSecret {
        TotpSecret::from_bytes(*b"12345678901234567890")
    }

    #[test]
    fn codes_are_rfc_6238s_last_six_digits() {
        // RFC 6238 appendix B, SHA-1: the eight-digit values' last six.
        let vectors = [
            (59, 287_082),
            (1_111_111_109, 81_804),
            (1_111_111_111, 50_471),
            (1_234_567_890, 5_924),
            (2_000_000_000, 279_037),
        ];
        for (seconds, code) in vectors {
            assert_eq!(rfc().code(seconds / STEP_SECONDS), code, "at {seconds}");
        }
    }

    #[test]
    fn a_code_is_accepted_once_within_a_step_of_skew() {
        let at = |s: u64| UNIX_EPOCH + Duration::from_secs(s);
        let code = format!("{:06}", rfc().code(1_111_111_109 / STEP_SECONDS));
        let step = rfc().accept(&code, at(1_111_111_109), None).unwrap();
        assert_eq!(rfc().accept(&code, at(1_111_111_109), Some(step)), None, "spent");
        assert!(rfc().accept(&code, at(1_111_111_109 + 30), None).is_some(), "one step late");
        assert_eq!(rfc().accept(&code, at(1_111_111_109 + 90), None), None, "three steps late");
        assert_eq!(rfc().accept("12345", at(59), None), None);
        assert_eq!(rfc().accept("abcdef", at(59), None), None);
    }

    #[test]
    fn the_uri_is_what_authenticator_apps_read() {
        let uri = rfc().uri("Korpis Host", "friend@example.com");
        assert_eq!(
            uri,
            "otpauth://totp/Korpis%20Host:friend%40example.com?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=Korpis%20Host&algorithm=SHA1&digits=6&period=30"
        );
    }
}
