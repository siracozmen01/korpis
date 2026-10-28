// SPDX-License-Identifier: AGPL-3.0-or-later

//! Migration pass tickets (§8.1 of docs/05-scheduling.md): what lets a migration's source write
//! one pass of a volume into the destination's staging of it. The control plane puts one in both
//! nodes' roles for each pass, and the destination's agent checks it offline with the token keys
//! it holds, so a pass under way survives the control plane's absence until the ticket expires.
//!
//! A ticket names one pass: the migration, the volume, the node sending, the node receiving, the
//! pass, the pass it is an increment from (none for a whole one, or for a tree, which sends what
//! differs), and its expiry. It is a ticket of identifiers (`stamped`), prefixed `korpismt_` and
//! signed over a purpose label of its own, so a replica's transfer ticket is never taken for
//! one, nor one for a transfer's.

use super::stamped::Kind;
use super::token::{KeyRing, SigningKey};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

pub use super::sealed::SealError as TicketError;

/// Every ticket starts with this.
pub const PREFIX: &str = "korpismt_";
/// The kind: its prefix, and what the signature is over before the payload.
const KIND: Kind = Kind { prefix: PREFIX, purpose: b"korpis.migration.v1\0" };

/// How long a ticket works: the time one pass may take, as a replica's transfer may.
pub const LIFETIME: Duration = crate::workload::migration::PASS_LIFETIME;

/// One pass of one volume of a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    /// The migration.
    pub migration: Uuid,
    /// The volume.
    pub volume: Uuid,
    /// The node sending: the source.
    pub from: Uuid,
    /// The node receiving: the destination.
    pub to: Uuid,
    /// The pass.
    pub pass: Uuid,
    /// The pass it is an increment from, which the destination holds; `None` for a whole one.
    pub parent: Option<Uuid>,
    /// When it stops working.
    pub expires_at: SystemTime,
}

/// The ticket for `p`, signed by `key`.
#[must_use]
pub fn mint(key: &SigningKey, p: &Pass) -> String {
    let ids = [p.migration, p.volume, p.from, p.to, p.pass, p.parent.unwrap_or_default()];
    KIND.mint(key, ids, p.expires_at)
}

/// The pass `presented` names, if it is a ticket signed by a key in `keys` and not expired at
/// `now`.
pub fn open(keys: &KeyRing, presented: &str, now: SystemTime) -> Result<Pass, TicketError> {
    let ([migration, volume, from, to, pass, parent], expires_at) =
        KIND.open(keys, presented, now)?;
    let parent = (!parent.is_nil()).then_some(parent);
    Ok(Pass { migration, volume, from, to, pass, parent, expires_at })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn ring(key: &SigningKey) -> KeyRing {
        let mut keys = KeyRing::default();
        keys.insert(key.key_id, &key.public_bytes()).unwrap_or_default();
        keys
    }

    fn pass(now: SystemTime, parent: Option<Uuid>) -> Pass {
        Pass {
            migration: Uuid::now_v7(),
            volume: Uuid::now_v7(),
            from: Uuid::now_v7(),
            to: Uuid::now_v7(),
            pass: Uuid::now_v7(),
            parent,
            expires_at: now + LIFETIME,
        }
    }

    #[test]
    fn a_pass_opens_as_minted_until_it_expires_and_never_under_another_key() {
        let (key, other) = (SigningKey::generate(3), SigningKey::generate(4));
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        for parent in [None, Some(Uuid::now_v7())] {
            let p = pass(now, parent);
            let ticket = mint(&key, &p);
            assert!(ticket.len() <= KIND.encoded(6), "{}", ticket.len());
            assert_eq!(open(&ring(&key), &ticket, now), Ok(p));
            assert_eq!(open(&ring(&key), &ticket, p.expires_at), Err(TicketError::Expired));
            assert_eq!(open(&ring(&other), &ticket, now), Err(TicketError::Signature));
        }
    }

    #[test]
    fn a_transfer_s_ticket_is_never_a_pass_s_nor_a_pass_s_a_transfer_s() {
        use super::super::transfer;
        let key = SigningKey::generate(3);
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let t = transfer::Transfer {
            volume: Uuid::now_v7(),
            from: Uuid::now_v7(),
            to: Uuid::now_v7(),
            snapshot: Uuid::now_v7(),
            parent: None,
            expires_at: now + LIFETIME,
        };
        let theirs = transfer::mint(&key, &t);
        assert_eq!(open(&ring(&key), &theirs, now), Err(TicketError::Malformed));
        let ours = mint(&key, &pass(now, None));
        assert_eq!(transfer::open(&ring(&key), &ours, now), Err(TicketError::Malformed));
        // Even under the other's prefix, the purpose label it was signed over differs.
        let disguised = ours.replacen(PREFIX, transfer::PREFIX, 1);
        assert!(transfer::open(&ring(&key), &disguised, now).is_err());
    }
}
