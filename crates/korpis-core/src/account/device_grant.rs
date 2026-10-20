// SPDX-License-Identifier: AGPL-3.0-or-later

//! The CLI's sign-in (§9.4 of docs/08-identity.md): the OAuth device authorization grant of RFC
//! 8628. The CLI asks for a device authorization and is given a device code, which it keeps and
//! polls with, and a user code, which its user types into the panel of a browser already signed
//! in; the account approves it there, and the CLI's next poll begins a session of its own.

use crate::principal::Holder;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long a device authorization waits to be approved.
pub const DEVICE_GRANT_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// How often the CLI may poll, at first; each poll sooner adds five seconds (RFC 8628 §3.5).
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// What a poll sooner than its interval adds to it.
pub const SLOW_DOWN: Duration = Duration::from_secs(5);
/// The user code's characters: twenty consonants, no vowels to spell words with and none easily
/// mistaken for another (RFC 8628 §6.1).
pub const USER_CODE_ALPHABET: &[u8; 20] = b"BCDFGHJKLMNPQRSTVWXZ";
/// How many characters a user code has: 20^8, about 34 bits.
pub const USER_CODE_LENGTH: usize = 8;

/// A device authorization: a CLI's request to sign in, waiting for an account to decide it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorization {
    /// Its identifier.
    pub id: Uuid,
    /// What the user types, eight characters of [`USER_CODE_ALPHABET`].
    pub user_code: String,
    /// When the CLI asked.
    pub created_at: SystemTime,
    /// When it stops being approvable.
    pub expires_at: SystemTime,
    /// How long the CLI must wait between polls.
    pub interval: Duration,
    /// What the CLI said it was, shown to whoever is asked to approve it.
    pub user_agent: String,
    /// Where it asked from, shown with it: an address, or an IPv6 /64.
    pub source: String,
    /// What was decided, and by whom, once it was.
    pub decision: Option<Decision>,
}

/// The answer to a device authorization, by an account or by a person signed in through an
/// identity provider (§9.7 of docs/08-identity.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// Who decided, and who holds the session the CLI begins if it approved.
    pub holder: Holder,
    /// When.
    pub at: SystemTime,
    /// Whether it approved.
    pub approved: bool,
}

impl DeviceAuthorization {
    /// Whether it can still be decided or polled at `now`.
    #[must_use]
    pub fn live(&self, now: SystemTime) -> bool {
        now < self.expires_at
    }

    /// The user code as it is shown: two groups of four.
    #[must_use]
    pub fn shown(&self) -> String {
        shown(&self.user_code)
    }
}

/// `code` as it is shown: two groups of four, `BCDF-GHJK`.
#[must_use]
pub fn shown(code: &str) -> String {
    match (code.get(..4), code.get(4..)) {
        (Some(a), Some(b)) => format!("{a}-{b}"),
        _ => code.to_owned(),
    }
}

/// A user code as typed, upper-cased with its separators and spaces taken out, if it is one.
#[must_use]
pub fn user_code(typed: &str) -> Option<String> {
    let code: String =
        typed.chars().filter(|c| !matches!(c, '-' | ' ')).map(|c| c.to_ascii_uppercase()).collect();
    let valid =
        code.len() == USER_CODE_LENGTH && code.bytes().all(|b| USER_CODE_ALPHABET.contains(&b));
    valid.then_some(code)
}

/// A fresh user code.
pub fn new_user_code() -> Result<String, super::EntropyError> {
    loop {
        if let Some(code) = draw(super::nonce()?.into_iter()) {
            return Ok(code);
        }
    }
}

/// A user code drawn from `bytes`, one byte per character, each drawn without bias; none if
/// they run out first, which 32 random bytes do about once in 10^20 times.
fn draw(bytes: impl Iterator<Item = u8>) -> Option<String> {
    // 240 is the largest multiple of 20 a byte holds: beyond it, drawing again keeps every
    // character equally likely.
    let code: String = bytes
        .filter(|b| *b < 240)
        .filter_map(|b| USER_CODE_ALPHABET.get(usize::from(b % 20)).map(|c| char::from(*c)))
        .take(USER_CODE_LENGTH)
        .collect();
    (code.len() == USER_CODE_LENGTH).then_some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_code_is_read_however_it_is_typed_and_only_from_its_alphabet() {
        assert_eq!(user_code("bcdf-ghjk").as_deref(), Some("BCDFGHJK"));
        assert_eq!(user_code(" BCDF GHJK ").as_deref(), Some("BCDFGHJK"));
        assert_eq!(user_code("ABCD-EFGH"), None, "vowels are not in it");
        assert_eq!(user_code("BCDF-GHJ"), None);
        assert_eq!(shown("BCDFGHJK"), "BCDF-GHJK");
    }

    #[test]
    fn a_fresh_user_code_draws_again_past_the_last_multiple_of_twenty() {
        let bytes = [255u8, 240, 0, 19, 20, 39, 1, 2, 3, 4];
        assert_eq!(draw(bytes.into_iter()).as_deref(), Some("BZBZCDFG"));
        assert_eq!(draw([0u8; 7].into_iter()), None, "seven bytes are not a code");
        let fresh = new_user_code().unwrap();
        assert_eq!(user_code(&fresh).as_deref(), Some(fresh.as_str()));
    }
}
