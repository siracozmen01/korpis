// SPDX-License-Identifier: AGPL-3.0-or-later

//! A grant as it is kept: the grant evaluation reads, and its provenance (§3 of
//! docs/08-identity.md).

use super::grant::Grant;
use crate::principal::Principal;
use std::time::SystemTime;

/// A stored grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRecord {
    /// What evaluation reads.
    pub grant: Grant,
    /// The principal it is held by, in structured form; `grant.subject` is its subject.
    pub holder: Principal,
    /// Who issued it.
    pub issued_by: Principal,
    /// When it was issued.
    pub issued_at: SystemTime,
    /// When it was revoked, if it was. Revocation is written on this record only and derived
    /// for its descendants (§3.5).
    pub revoked_at: Option<SystemTime>,
    /// When it was suspended, if it is.
    pub suspended_at: Option<SystemTime>,
    /// What it is for, for people; no decision reads it.
    pub label: String,
}

/// What a grant's state is, as a client is told (§3 of docs/08-identity.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantState {
    /// In force.
    Issued,
    /// Past its expiry.
    Expired,
    /// Every use taken.
    Exhausted,
    /// Revoked on this record.
    Revoked,
    /// Suspended on this record.
    Suspended,
}

impl GrantRecord {
    /// The record's own state at `now`. Ancestors are not consulted: a grant whose ancestor is
    /// revoked shows as issued and is refused at use, which is what §3.5 says.
    #[must_use]
    pub fn state(&self, now: SystemTime) -> GrantState {
        let c = &self.grant.conditions;
        if self.revoked_at.is_some() {
            GrantState::Revoked
        } else if self.suspended_at.is_some() {
            GrantState::Suspended
        } else if c.expires_at.is_some_and(|e| e <= now) {
            GrantState::Expired
        } else if c.max_uses.is_some_and(|m| self.grant.uses >= m) {
            GrantState::Exhausted
        } else {
            GrantState::Issued
        }
    }
}

/// Whether a chain, root first, is in force at `now`: every record in it issued, none revoked,
/// suspended, expired, or exhausted, which is the cascade of §3.5 of docs/08-identity.md derived
/// for its leaf. It says nothing of what the chain permits.
#[must_use]
pub fn in_force(chain: &[GrantRecord], now: SystemTime) -> bool {
    !chain.is_empty() && chain.iter().all(|r| r.state(now) == GrantState::Issued)
}
