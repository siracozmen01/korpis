// SPDX-License-Identifier: AGPL-3.0-or-later

//! User sessions (§9.4 of docs/08-identity.md): what a sign-in begins, and what a refresh
//! credential exchanges for access tokens until it ends, by idleness, by age, or by its user. A
//! session is held by an account, or by a person who signed in through an identity provider
//! (§9.7), with the sets the provider named then.

pub use crate::principal::Holder;
use crate::principal::SetName;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long a session lasts without an exchange, unless the operator says otherwise.
pub const SESSION_IDLE: Duration = Duration::from_secs(12 * 3600);
/// How long any session lasts, unless the operator says otherwise.
pub const SESSION_MAX: Duration = Duration::from_secs(7 * 24 * 3600);
/// How recently a session must have signed in, or re-authenticated, to change its account's
/// factors (§9.6 of docs/08-identity.md).
pub const REAUTHENTICATE_WITHIN: Duration = Duration::from_secs(10 * 60);
/// The longest user agent kept with a session, in characters.
pub const MAX_USER_AGENT: usize = 256;

/// How long sessions last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLimits {
    /// Without an exchange.
    pub idle: Duration,
    /// In all.
    pub max: Duration,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self { idle: SESSION_IDLE, max: SESSION_MAX }
    }
}

/// Where a session's refresh credential is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    /// A browser, in a cookie no script can read.
    Web,
    /// The CLI, in the operating system's keychain.
    Cli,
}

impl SessionKind {
    /// As stored.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Cli => "cli",
        }
    }

    /// From its stored form.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Web, Self::Cli].into_iter().find(|k| k.as_str() == s)
    }
}

/// A session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSession {
    /// Its identifier, which access tokens name.
    pub id: Uuid,
    /// Who signed in.
    pub holder: Holder,
    /// For a person, the sets their provider named at the sign-in, which count only while their
    /// record as a member of each vouches (§9.7); none for an account.
    pub sets: Vec<SetName>,
    /// Where its refresh credential is held.
    pub kind: SessionKind,
    /// When it began.
    pub created_at: SystemTime,
    /// When its refresh credential was last exchanged.
    pub last_used_at: SystemTime,
    /// When a factor was last checked for it: at sign-in, or a later confirmation.
    pub authenticated_at: SystemTime,
    /// When it ends whatever is done with it.
    pub expires_at: SystemTime,
    /// The user agent that began it, as it said.
    pub user_agent: String,
    /// The address it began from, as its source is charged (an address, or an IPv6 /64).
    pub source: String,
    /// The portal it was begun at, beneath whose organization it acts (§9.8 of
    /// docs/08-identity.md); none for one begun at the installation's own address.
    pub portal: Option<AtPortal>,
    /// The organization beneath which it acts, wherever it is used: its portal's, or the one its
    /// password and second factor were both set beneath, the narrower where both (§9.8 of
    /// docs/08-identity.md); none for one that reaches whatever its holder's grants do.
    pub within: Option<Uuid>,
}

/// A portal a session was begun at, and the organization it is confined beneath.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtPortal {
    /// The portal.
    pub portal: Uuid,
    /// Its organization.
    pub organization: Uuid,
}

impl UserSession {
    /// Whether it is still live at `now`, ending after `idle` without an exchange.
    #[must_use]
    pub fn live(&self, now: SystemTime, idle: Duration) -> bool {
        now < self.expires_at && self.last_used_at.checked_add(idle).is_some_and(|end| now < end)
    }
}

/// `agent` cut to what a session keeps of it, control characters dropped.
#[must_use]
pub fn user_agent(agent: &str) -> String {
    agent.chars().filter(|c| !c.is_control()).take(MAX_USER_AGENT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_ends_when_idle_or_old() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let s = UserSession {
            id: Uuid::nil(),
            holder: Holder::Account(Uuid::nil()),
            sets: Vec::new(),
            kind: SessionKind::Web,
            created_at: at,
            last_used_at: at,
            authenticated_at: at,
            expires_at: at + SESSION_MAX,
            user_agent: String::new(),
            source: String::new(),
            portal: None,
            within: None,
        };
        assert!(s.live(at + SESSION_IDLE - Duration::from_secs(1), SESSION_IDLE));
        assert!(!s.live(at + SESSION_IDLE, SESSION_IDLE), "idle");
        let used = UserSession { last_used_at: at + SESSION_MAX, ..s.clone() };
        assert!(!used.live(at + SESSION_MAX, SESSION_IDLE), "old");
        assert_eq!(user_agent("Firefox\n\u{7}/1"), "Firefox/1");
        assert_eq!(user_agent(&"x".repeat(300)).len(), MAX_USER_AGENT);
    }
}
