// SPDX-License-Identifier: AGPL-3.0-or-later

//! Certificates the ingress terminates TLS with (§3.2 of docs/07-networking.md, §3.6 of
//! docs/01-model.md). A certificate is its organization's, for the names its leaf names; an
//! endpoint serves with one only if it covers every name the endpoint routes and is valid now.
//! Reading a chain and matching its key is the adapter's (a port of the application); what the
//! leaf says, and what follows from it, is decided here.

use super::domain::DomainName;
use crate::event::Severity;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

mod node_proof;

pub use node_proof::NodeProof;

/// The most names one certificate is for, as ACME's directories bound an order.
pub const MAX_NAMES: usize = 100;

/// Where a certificate comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Issued and renewed by the control plane from an ACME directory.
    Acme,
    /// Uploaded by its organization, chain and key.
    Provided,
}

impl Source {
    /// The stored and wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Acme => "acme",
            Self::Provided => "provided",
        }
    }

    /// The source a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "acme" => Self::Acme,
            "provided" => Self::Provided,
            _ => return None,
        })
    }
}

/// Where a certificate is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Being obtained; no chain yet.
    Pending,
    /// Held, chain and key.
    Issued,
    /// Its issuance failed, for the reason it records.
    Failed,
}

impl State {
    /// The stored and wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Issued => "issued",
            Self::Failed => "failed",
        }
    }

    /// The state a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "issued" => Self::Issued,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

/// What a chain's leaf says of itself, as the adapter reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    /// The DNS names of its subject alternative names, lowercase, wildcards as `*.` names.
    pub names: Vec<String>,
    /// When it becomes valid.
    pub not_before: SystemTime,
    /// When it stops being valid.
    pub not_after: SystemTime,
    /// Its key's identifier: the SHA-256 of its subject public key info, in lowercase hex. Two
    /// chains share it exactly when they share a key.
    pub key_id: String,
}

/// Why a leaf cannot be a certificate here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LeafError {
    /// It names no DNS name, or more than [`MAX_NAMES`].
    #[error("a certificate names 1 to {MAX_NAMES} DNS names; this one names {0}")]
    Names(usize),
    /// A name is neither a DNS name nor a wildcard over one.
    #[error("{0:?} is not a DNS name, nor a wildcard of one label over one")]
    Name(String),
    /// It has already expired, or its validity ends before it begins.
    #[error("it has expired, or its validity ends before it begins")]
    Expired,
}

/// Whether `pattern`, a certificate's name, covers `name`: the same name, or a wildcard whose
/// `*` stands for exactly one label of it, as RFC 6125 matches.
#[must_use]
pub fn matches(pattern: &str, name: &DomainName) -> bool {
    match pattern.strip_prefix("*.") {
        Some(parent) => name.as_str().split_once('.').is_some_and(|(_, rest)| rest == parent),
        None => pattern == name.as_str(),
    }
}

impl Leaf {
    /// Checks that it can be a certificate at `now`: it names 1 to [`MAX_NAMES`] names, each a
    /// DNS name or a wildcard over one, and it has not expired.
    pub fn check(&self, now: SystemTime) -> Result<(), LeafError> {
        if self.names.is_empty() || self.names.len() > MAX_NAMES {
            return Err(LeafError::Names(self.names.len()));
        }
        for n in &self.names {
            let exact = n.strip_prefix("*.").unwrap_or(n);
            if DomainName::parse(exact).is_none() {
                return Err(LeafError::Name(n.clone()));
            }
        }
        if self.not_after <= now || self.not_after <= self.not_before {
            return Err(LeafError::Expired);
        }
        Ok(())
    }
}

/// A certificate and its key, the key sealed and kept apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certificate {
    /// Its identifier.
    pub id: Uuid,
    /// Its organization.
    pub organization: Uuid,
    /// The names it is for: its leaf's, or, while pending, those it is asked for.
    pub names: Vec<String>,
    /// Where it comes from.
    pub source: Source,
    /// Its chain in PEM, leaf first; empty while pending.
    pub chain: String,
    /// When its leaf becomes valid, once issued.
    pub not_before: Option<SystemTime>,
    /// When its leaf stops being valid, once issued.
    pub not_after: Option<SystemTime>,
    /// Where it is in its life.
    pub state: State,
    /// Why it failed, for `Failed`.
    pub reason: Option<String>,
    /// For `Acme`: when its renewal begins.
    pub renew_after: Option<SystemTime>,
    /// Raised whenever its chain and key change, so a node serving it knows to fetch it again.
    pub revision: i64,
    /// When it was created.
    pub created_at: SystemTime,
    /// For `Acme`: the endpoint whose names it is for.
    pub owner: Option<Owner>,
    /// For `Acme`: the portal whose name it is for, where no endpoint's names are (§3.6 of
    /// docs/07-networking.md).
    pub portal: Option<Uuid>,
    /// For `Acme`: how many attempts in a row to issue it failed.
    pub attempts: u32,
    /// For `Acme`: when the next attempt may be made, after one failed.
    pub next_attempt: Option<SystemTime>,
    /// Its key's identifier, once it holds one ([`Leaf::key_id`]).
    pub key_id: Option<String>,
    /// Whether its key was given to a node later removed as compromised (§9 of
    /// docs/17-security.md), until a chain with another key replaces it.
    pub exposed: bool,
}

/// The endpoint an `acme` certificate is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    /// The workload.
    pub workload: Uuid,
    /// The endpoint, by name.
    pub endpoint: String,
}

/// A TLS-ALPN-01 challenge a node's proxy answers on port 443 while an order runs: `name`,
/// proven by a certificate made for it carrying `digest` in its `acmeIdentifier` extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlpnChallenge {
    /// The name proven.
    pub name: String,
    /// The SHA-256 digest of the key authorization.
    pub digest: Vec<u8>,
}

/// An HTTP-01 challenge a node's proxy answers on port 80 while an order runs: the key
/// authorization at `/.well-known/acme-challenge/<token>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpChallenge {
    /// The token.
    pub token: String,
    /// What is answered.
    pub key_authorization: String,
}

/// How long the issuer waits before attempt `attempts + 1`, after `attempts` failed in a row:
/// five minutes, doubling, at most a day, which keeps an installation within a directory's
/// limits on failed validations (Let's Encrypt allows five an hour for a name).
#[must_use]
pub fn backoff(attempts: u32) -> Duration {
    let minutes = 5u64.saturating_mul(1u64 << attempts.saturating_sub(1).min(16));
    Duration::from_secs(minutes.saturating_mul(60)).min(Duration::from_secs(86_400))
}

impl Certificate {
    /// The first of `names` it does not cover, if any.
    #[must_use]
    pub fn uncovered<'a>(&self, names: &'a [DomainName]) -> Option<&'a DomainName> {
        names.iter().find(|n| !self.names.iter().any(|p| matches(p, n)))
    }

    /// Whether it can be served at `now`: issued, and within its validity.
    #[must_use]
    pub fn valid_at(&self, now: SystemTime) -> bool {
        self.state == State::Issued
            && self.not_before.is_some_and(|b| b <= now)
            && self.not_after.is_some_and(|a| now < a)
    }
}

/// When a certificate valid from `not_before` to `not_after` is renewed: two thirds of the way
/// through its life (§3.2 of docs/07-networking.md).
#[must_use]
pub fn renew_after(not_before: SystemTime, not_after: SystemTime) -> SystemTime {
    let life = not_after.duration_since(not_before).unwrap_or(Duration::ZERO);
    not_before + life / 3 * 2
}

/// How near `not_after` a certificate valid from `not_before` is at `now`, as an event says it:
/// a warning once five sixths of its life have passed, critical once seventeen eighteenths have.
/// Measured in its own life rather than in days, so a certificate of six days and one of a year
/// are each told in time to act and neither is told all its life; an `acme` one, renewed at two
/// thirds, is told only when its renewal is half its window late.
#[must_use]
pub fn expiry_alert(
    not_before: SystemTime,
    not_after: SystemTime,
    now: SystemTime,
) -> Option<Severity> {
    let life = not_after.duration_since(not_before).unwrap_or(Duration::ZERO);
    let left = not_after.duration_since(now).unwrap_or(Duration::ZERO);
    if left <= life / 18 {
        Some(Severity::Critical)
    } else if left <= life / 6 {
        Some(Severity::Warning)
    } else {
        None
    }
}

/// A certificate came a step closer to expiring: an event, keyed to the certificate, in its
/// organization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiryAlert {
    /// The certificate.
    pub certificate: Uuid,
    /// Its organization.
    pub organization: Uuid,
    /// The names it covers.
    pub names: Vec<String>,
    /// Where it came from.
    pub source: Source,
    /// When it expires.
    pub not_after: SystemTime,
    /// How close that is.
    pub severity: Severity,
    /// When it was decided, on the control plane's clock.
    pub at: SystemTime,
}

impl ExpiryAlert {
    /// The event's kind.
    pub const KIND: &'static str = "certificate.expiring";

    /// The event's key: one ordered history per certificate.
    #[must_use]
    pub fn key(&self) -> String {
        format!("certificate:{}", self.certificate)
    }
}

#[cfg(test)]
mod tests;
