// SPDX-License-Identifier: AGPL-3.0-or-later

//! What Korpis asks an identity provider, and what it accepts as an answer (§5 of
//! docs/08-identity.md). A provider re-verifies what it vouched for, and says who presented a
//! piece of evidence; both answers come up its own authenticated stream (§3.3 of
//! docs/16-extensions.md), so nothing it says needs a signature of its own.

use crate::principal::SetName;
use std::collections::BTreeMap;
use std::time::SystemTime;

/// The most people one re-verification asks about.
pub const MAX_MEMBERS: usize = 256;
/// The most sets one assertion names.
pub const MAX_SETS: usize = 256;
/// The most evidence one assertion is asked about, in bytes.
pub const MAX_EVIDENCE: usize = 64 << 10;
/// The most headers that evidence carries.
pub const MAX_HEADERS: usize = 32;

/// A re-verification (§5.1 of docs/08-identity.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembersAsked {
    /// The set; `None` asks only whether the people are still known.
    pub set: Option<SetName>,
    /// The people, by their identifier at the provider.
    pub members: Vec<String>,
    /// Other sets each must also be in.
    pub require_membership: Vec<SetName>,
}

/// The answer to a [`MembersAsked`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Members {
    /// For a set: it exists and is the set it was.
    pub set_exists: bool,
    /// The people confirmed.
    pub present: Vec<String>,
    /// The people the provider holds no current evidence of: each keeps their last
    /// confirmation, to lapse on its own (§5.1). Every one asked about and named in neither
    /// list is gone.
    pub unknown: Vec<String>,
}

/// Evidence a person presented to a provider's surface, passed on as received (§5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// Its body, opaque to Korpis.
    pub body: Vec<u8>,
    /// How it arrived.
    pub headers: BTreeMap<String, String>,
}

impl Evidence {
    /// Whether it is within what a provider is asked about.
    #[must_use]
    pub fn bounded(&self) -> bool {
        self.body.len() <= MAX_EVIDENCE && self.headers.len() <= MAX_HEADERS
    }
}

/// Who presented a piece of evidence, as the provider vouches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    /// The person, by their identifier at the provider.
    pub user: String,
    /// The provider's sets they are in.
    pub sets: Vec<SetName>,
    /// The provider's identifier for this one presentation, accepted once.
    pub presentation: String,
    /// When it was presented.
    pub at: SystemTime,
}

fn part(s: &str) -> bool {
    !s.is_empty() && !s.contains(':')
}

impl MembersAsked {
    /// Whether `m` answers it: it names only people asked about.
    #[must_use]
    pub fn answered_by(&self, m: &Members) -> bool {
        let asked = |v: &[String]| v.iter().all(|p| self.members.contains(p));
        m.present.len() + m.unknown.len() <= self.members.len()
            && asked(&m.present)
            && asked(&m.unknown)
            && !m.unknown.iter().any(|u| m.present.contains(u))
    }
}

/// A sign-in the control plane began (§9.7 of docs/08-identity.md), which a provider that signs
/// people in answers with where to send the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignIn {
    /// Opaque; returned with the person.
    pub state: String,
    /// Opaque; asserted back inside what the person returns with.
    pub nonce: String,
    /// The PKCE challenge of a verifier only the panel holds, `S256`.
    pub code_challenge: String,
    /// The panel's callback.
    pub redirect_uri: String,
}

/// Where a person is sent to sign in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorization {
    /// An `https` URL at the identity provider.
    pub url: String,
}

impl Authorization {
    /// Whether it can be sent to a browser: an `https` URL with a host, and no longer than a
    /// browser keeps.
    #[must_use]
    pub fn well_formed(&self) -> bool {
        const MAX_URL: usize = 8 << 10;
        self.url.len() <= MAX_URL
            && self.url.strip_prefix("https://").is_some_and(|rest| {
                rest.split(['/', '?', '#']).next().is_some_and(|h| !h.is_empty())
            })
    }
}

impl Assertion {
    /// Whether it can be believed as an answer: it names a person, a presentation, and sets a
    /// subject key can hold.
    #[must_use]
    pub fn well_formed(&self) -> bool {
        !self.user.is_empty()
            && !self.presentation.is_empty()
            && self.sets.len() <= MAX_SETS
            && self.sets.iter().all(|s| part(&s.realm) && !s.member_of.is_empty())
    }
}
