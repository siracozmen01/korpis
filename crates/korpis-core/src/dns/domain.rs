// SPDX-License-Identifier: AGPL-3.0-or-later

//! Domains an organization holds (§3.2 of docs/07-networking.md, §3.6 of docs/01-model.md): a
//! name covers itself and every name beneath it; a verified domain of one organization may not
//! overlap another's, except one delegated beneath it; and a name is routable by the organization
//! holding the most specific verified domain that covers it, which is what lets a delegated name
//! be its holder's and not its parent's.

use std::time::SystemTime;
use uuid::Uuid;

/// The label beneath a domain whose TXT record carries its verification token.
pub const VERIFY_LABEL: &str = "_korpis-verify";

/// A DNS name a domain or a route names: lowercase dotted labels of letters, digits, and inner
/// hyphens, at least two of them, at most 253 bytes, with no final dot and no wildcard, and a last
/// label that is not all digits, since no top-level domain is and a name that ends in one is an
/// IPv4 address, which TLS never names by SNI (RFC 3696 §2, RFC 6066 §3). A name in another script
/// is written in its ASCII form (`xn--…`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DomainName(String);

impl DomainName {
    /// `s` as a name, if it is one.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let label = |l: &str| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        };
        let top = s.rsplit('.').next().is_some_and(|t| !t.bytes().all(|b| b.is_ascii_digit()));
        let ok = s.len() <= 253 && s.contains('.') && top && s.split('.').all(label);
        ok.then(|| Self(s.to_owned()))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this name covers `other`: is it, or lies above it.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        other.0 == self.0
            || other.0.strip_suffix(self.0.as_str()).is_some_and(|rest| rest.ends_with('.'))
    }

    /// Whether either covers the other.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.covers(other) || other.covers(self)
    }

    /// Where the verification token is published: `_korpis-verify.<name>`.
    #[must_use]
    pub fn verification_record(&self) -> String {
        format!("{VERIFY_LABEL}.{}", self.0)
    }
}

/// A new verification token: 24 random bytes in hex, which no one can guess before it is shown.
///
/// # Errors
/// The system has no randomness to give.
#[cfg(feature = "authz")]
pub fn new_token() -> Result<String, crate::authz::credential::EntropyError> {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes)
        .map_err(|e| crate::authz::credential::EntropyError(e.to_string()))?;
    Ok(bytes.iter().fold(String::with_capacity(48), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// A name the ingress routes, and to what (§3.2 of docs/07-networking.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The name.
    pub name: DomainName,
    /// The organization routing it, the workload's.
    pub organization: Uuid,
    /// The workload whose endpoint it routes to.
    pub workload: Uuid,
    /// The endpoint, by name.
    pub endpoint: String,
    /// The certificate it is served with, once the endpoint's TLS names or is given one.
    pub certificate: Option<Uuid>,
}

/// How a domain is verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// By its token in a TXT record the control plane resolves.
    DnsTxt,
    /// Beneath a verified domain of an ancestor organization, by that domain's holder.
    Delegated,
    /// Attested by an operator of the installation.
    Operator,
}

impl Verification {
    /// The stored and wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DnsTxt => "dns_txt",
            Self::Delegated => "delegated",
            Self::Operator => "operator",
        }
    }

    /// The method a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "dns_txt" => Self::DnsTxt,
            "delegated" => Self::Delegated,
            "operator" => Self::Operator,
            _ => return None,
        })
    }
}

/// A domain an organization holds, or has asked to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain {
    /// Its identifier.
    pub id: Uuid,
    /// Its holder.
    pub organization: Uuid,
    /// The name it covers.
    pub name: DomainName,
    /// How it is verified.
    pub verification: Verification,
    /// What the TXT record must hold, for `DnsTxt`.
    pub token: String,
    /// For `Delegated`: the domain it lies beneath.
    pub parent: Option<Uuid>,
    /// When it was verified; `None` while pending.
    pub verified_at: Option<SystemTime>,
    /// When it was asked for.
    pub created_at: SystemTime,
}

impl Domain {
    /// Whether it is verified.
    #[must_use]
    pub fn verified(&self) -> bool {
        self.verified_at.is_some()
    }
}

/// The verified domain of another organization that keeps `candidate` from being verified, if
/// any: one overlapping it that is not among `chain`, the domains `candidate` was delegated
/// beneath, nearest first.
#[must_use]
pub fn blocked_by<'a>(
    candidate: &Domain,
    others: &'a [Domain],
    chain: &[Uuid],
) -> Option<&'a Domain> {
    others.iter().find(|d| {
        d.verified()
            && d.organization != candidate.organization
            && d.name.overlaps(&candidate.name)
            && !chain.contains(&d.id)
    })
}

/// The organization that may route `name`: the holder of the most specific verified domain among
/// `verified` that covers it.
#[must_use]
pub fn holder_of(name: &DomainName, verified: &[Domain]) -> Option<Uuid> {
    verified
        .iter()
        .filter(|d| d.verified() && d.name.covers(name))
        .max_by_key(|d| d.name.as_str().len())
        .map(|d| d.organization)
}

#[cfg(test)]
mod tests;
