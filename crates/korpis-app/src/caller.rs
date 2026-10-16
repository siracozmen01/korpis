// SPDX-License-Identifier: AGPL-3.0-or-later

//! The authenticated caller of a use case.

use korpis_core::authz::Subject;
use korpis_core::principal::Principal;
use std::net::IpAddr;

/// Who is asking, and how they reached us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Who, in structured form; effects record this.
    pub principal: Principal,
    /// Who, as authorization compares it.
    pub subject: Subject,
    /// Where the request came from, for network conditions.
    pub source_ip: Option<IpAddr>,
    /// Whether the credential carried a second factor. API tokens never do; a signed-in user's
    /// access token always does, since no sign-in is one factor (§9.6 of docs/08-identity.md).
    pub mfa: bool,
    /// The session a signed-in user's access token was exchanged under.
    pub session: Option<uuid::Uuid>,
}

impl Caller {
    /// A caller acting as `principal`.
    #[must_use]
    pub fn new(principal: Principal, source_ip: Option<IpAddr>, mfa: bool) -> Self {
        Self { subject: principal.subject(), principal, source_ip, mfa, session: None }
    }

    /// The caller, acting under the user session `session`.
    #[must_use]
    pub const fn with_session(mut self, session: uuid::Uuid) -> Self {
        self.session = Some(session);
        self
    }
}
