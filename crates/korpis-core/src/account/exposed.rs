// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where an account's shared secrets were set (§9.8 of docs/08-identity.md, Finding 133 of
//! docs/23-walkthroughs.md). A password, a TOTP secret, and recovery codes serve every address,
//! and one set through a session confined beneath an organization was set where whoever serves
//! that organization's portal can see it; so each records that organization, and a sign-in with
//! two secrets one holder may know both of reaches no further than that holder answers for.

use uuid::Uuid;

/// One of an account's shared secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shared {
    /// The password.
    Password,
    /// The TOTP secret.
    Totp,
    /// The recovery codes.
    RecoveryCodes,
}

/// For each shared secret of an account, the organization beneath which the session that set it
/// was confined; none for one set by a session that was not, or never set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SetWithin {
    /// The password's.
    pub password: Option<Uuid>,
    /// The TOTP secret's.
    pub totp: Option<Uuid>,
    /// The recovery codes'.
    pub recovery_codes: Option<Uuid>,
}

impl SetWithin {
    /// The organizations that the two secrets a sign-in with the password and `second` used
    /// were set beneath, when both were: only then may one holder know both.
    #[must_use]
    pub fn of_sign_in(&self, second: Shared) -> Option<(Uuid, Uuid)> {
        let other = match second {
            Shared::Totp => self.totp,
            Shared::RecoveryCodes => self.recovery_codes,
            Shared::Password => None,
        };
        self.password.zip(other)
    }

    /// The record of `secret`.
    #[must_use]
    pub const fn of(&self, secret: Shared) -> Option<Uuid> {
        match secret {
            Shared::Password => self.password,
            Shared::Totp => self.totp,
            Shared::RecoveryCodes => self.recovery_codes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sign_in_is_confined_only_by_two_secrets_both_set_beneath_an_organization() {
        let (pinecone, northwind) = (Uuid::now_v7(), Uuid::now_v7());
        let at = |password, totp, recovery_codes| SetWithin { password, totp, recovery_codes };
        assert_eq!(at(None, None, None).of_sign_in(Shared::Totp), None);
        assert_eq!(at(Some(pinecone), None, None).of_sign_in(Shared::Totp), None, "TOTP unseen");
        let both = at(Some(pinecone), Some(northwind), None);
        assert_eq!(both.of_sign_in(Shared::Totp), Some((pinecone, northwind)));
        assert_eq!(both.of_sign_in(Shared::RecoveryCodes), None, "codes shown nowhere");
        assert_eq!(both.of(Shared::Totp), Some(northwind));
    }
}
