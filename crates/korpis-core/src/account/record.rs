// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the control plane keeps of an account and its factors (§9.6 of docs/08-identity.md).

use crate::webauthn::Algorithm;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long an invitation lasts unless its issuer says less.
pub const INVITATION_LIFETIME: Duration = Duration::from_secs(7 * 24 * 3600);
/// How long the bootstrap invitation of an installation lasts (§9.1 of docs/08-identity.md).
pub const BOOTSTRAP_LIFETIME: Duration = Duration::from_secs(3600);
/// How long a `WebAuthn` ceremony may take, from its challenge to its answer.
pub const CHALLENGE_LIFETIME: Duration = Duration::from_secs(300);

/// An account: the subject `user:<id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Its identifier, the user's.
    pub id: Uuid,
    /// How it is shown.
    pub display_name: String,
    /// What a password sign-in names it by, chosen when its invitation is redeemed.
    pub name: Option<String>,
    /// The principal key of whoever invited it.
    pub created_by: String,
    /// When it was created.
    pub created_at: SystemTime,
    /// When it was disabled, if it was: it signs in no more.
    pub disabled_at: Option<SystemTime>,
}

/// The longest a display name may be, in characters.
pub const MAX_DISPLAY_NAME: usize = 100;

/// Why a name is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// A display name is empty, too long, or holds a control character.
    #[error("a display name is 1 to 100 characters, none of them control")]
    Display,
    /// A sign-in name is not 3 to 64 of `a-z`, `0-9`, `.`, `_`, `-`, starting and ending with a
    /// letter or digit.
    #[error(
        "a sign-in name is 3 to 64 of a-z, 0-9, '.', '_', '-', starting and ending with a \
         letter or digit"
    )]
    SignIn,
}

/// `name`, checked as a display name and trimmed.
pub fn display_name(name: &str) -> Result<String, NameError> {
    let name = name.trim();
    let count = name.chars().count();
    if count == 0 || count > MAX_DISPLAY_NAME || name.chars().any(char::is_control) {
        return Err(NameError::Display);
    }
    Ok(name.to_owned())
}

/// `name`, checked as a sign-in name and lowercased, since a person types it from memory.
pub fn sign_in_name(name: &str) -> Result<String, NameError> {
    let name = name.trim().to_ascii_lowercase();
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let inner = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c);
    let ok = (3..=64).contains(&name.len())
        && name.chars().all(inner)
        && edge(name.chars().next())
        && edge(name.chars().last());
    if ok { Ok(name) } else { Err(NameError::SignIn) }
}

/// An invitation, which registers an account's first factor once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    /// Its identifier.
    pub id: Uuid,
    /// The account it registers a factor for.
    pub account: Uuid,
    /// The principal key of whoever issued it.
    pub issued_by: String,
    /// When it stops working.
    pub expires_at: SystemTime,
    /// Whether only registering a passkey redeems it, as the bootstrap invitation and
    /// `recover-root`'s are (§9.1 and §9.2 of docs/08-identity.md).
    pub passkey_only: bool,
    /// When it was redeemed, if it was.
    pub redeemed_at: Option<SystemTime>,
}

impl Invitation {
    /// Whether it can still be redeemed at `now`.
    #[must_use]
    pub fn open(&self, now: SystemTime) -> bool {
        self.redeemed_at.is_none() && now < self.expires_at
    }
}

/// A registered passkey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passkey {
    /// The credential's identifier.
    pub credential_id: Vec<u8>,
    /// The account it signs in to.
    pub account: Uuid,
    /// Its COSE public key.
    pub public_key: Vec<u8>,
    /// Its algorithm.
    pub algorithm: Algorithm,
    /// Its signature counter, as last seen.
    pub sign_count: u32,
    /// Its authenticator model, all zeros when unknown.
    pub aaguid: [u8; 16],
    /// Whether it may be synced.
    pub backup_eligible: bool,
    /// What its holder calls it.
    pub label: String,
    /// When it was registered.
    pub created_at: SystemTime,
    /// When it last signed in.
    pub last_used_at: Option<SystemTime>,
}

/// What a `WebAuthn` challenge is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ceremony {
    /// Registering a passkey: for an account, or redeeming an invitation.
    Register,
    /// Signing in with one.
    SignIn,
}

impl Ceremony {
    /// As stored.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::SignIn => "sign_in",
        }
    }

    /// From its stored form.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Register, Self::SignIn].into_iter().find(|c| c.as_str() == s)
    }
}

/// A `WebAuthn` challenge, issued for one ceremony.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    /// Its identifier, which the client returns with the ceremony's answer.
    pub id: Uuid,
    /// What it is for.
    pub ceremony: Ceremony,
    /// The random bytes the authenticator signs over.
    pub challenge: [u8; 32],
    /// The account a registration adds a passkey to.
    pub account: Option<Uuid>,
    /// The invitation a registration redeems.
    pub invitation: Option<Uuid>,
    /// When it stops being answerable.
    pub expires_at: SystemTime,
}

/// The factors an account holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Factors {
    /// How many passkeys.
    pub passkeys: u32,
    /// Whether it has a password.
    pub password: bool,
    /// Whether it has TOTP.
    pub totp: bool,
    /// How many recovery codes are unused.
    pub recovery_codes: u32,
}

impl Factors {
    /// Whether anything can sign in to it.
    #[must_use]
    pub const fn any(&self) -> bool {
        self.passkeys > 0 || self.password
    }

    /// Whether a second factor beside a password is held.
    #[must_use]
    pub const fn second(&self) -> bool {
        self.passkeys > 0 || self.totp || self.recovery_codes > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked_as_the_store_checks_them() {
        assert_eq!(sign_in_name(" Friend.One ").as_deref(), Ok("friend.one"));
        assert_eq!(sign_in_name("ab"), Err(NameError::SignIn));
        assert_eq!(sign_in_name("-ab"), Err(NameError::SignIn));
        assert_eq!(sign_in_name("ab_"), Err(NameError::SignIn));
        assert_eq!(sign_in_name("a b"), Err(NameError::SignIn));
        assert_eq!(sign_in_name("çağrı"), Err(NameError::SignIn));
        assert!(sign_in_name(&"a".repeat(64)).is_ok());
        assert_eq!(sign_in_name(&"a".repeat(65)), Err(NameError::SignIn));
        assert_eq!(display_name("  Çağrı Ö. "), Ok("Çağrı Ö.".to_owned()));
        assert_eq!(display_name(" "), Err(NameError::Display));
        assert_eq!(display_name("a\nb"), Err(NameError::Display));
        assert_eq!(display_name(&"é".repeat(101)), Err(NameError::Display));
    }

    #[test]
    fn an_invitation_is_open_until_redeemed_or_expired() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let mut i = Invitation {
            id: Uuid::nil(),
            account: Uuid::nil(),
            issued_by: "system:bootstrap".into(),
            expires_at: now + BOOTSTRAP_LIFETIME,
            passkey_only: true,
            redeemed_at: None,
        };
        assert!(i.open(now));
        assert!(!i.open(now + BOOTSTRAP_LIFETIME));
        i.redeemed_at = Some(now);
        assert!(!i.open(now));
    }
}
