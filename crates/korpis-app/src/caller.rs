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
    /// Whether the credential carried a second factor. API tokens never do; passkeys arrive with
    /// the web client (§9 of docs/08-identity.md).
    pub mfa: bool,
}

impl Caller {
    /// A caller acting as `principal`.
    #[must_use]
    pub fn new(principal: Principal, source_ip: Option<IpAddr>, mfa: bool) -> Self {
        Self { subject: principal.subject(), principal, source_ip, mfa }
    }
}
