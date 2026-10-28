// SPDX-License-Identifier: AGPL-3.0-or-later

//! Transfer tickets (§2.2 of docs/06-storage.md): what lets one node write into another node's
//! replica of a volume. The control plane puts one in the sending node's assignment and the
//! receiving node's, and the receiving agent checks it offline with the token keys it holds,
//! so a transfer under way survives the control plane's absence until the ticket expires.
//!
//! A ticket names one transfer: the volume, the node sending, the node receiving, the snapshot
//! the stream carries, the one it is an increment from (none for a whole one), and its expiry. It
//! is a ticket of identifiers (`stamped`), prefixed `korpistt_` and signed over a purpose label
//! of its own, so nothing else the key signs is taken for one, and a ticket is taken for nothing
//! else: it carries no grant and no right.

use super::stamped::Kind;
use super::token::{KeyRing, SigningKey};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

pub use super::sealed::SealError as TicketError;
use TicketError as SealError;

/// Every ticket starts with this.
pub const PREFIX: &str = "korpistt_";
/// The kind: its prefix, and what the signature is over before the payload.
const KIND: Kind = Kind { prefix: PREFIX, purpose: b"korpis.transfer.v1\0" };
/// The longest ticket.
#[cfg(test)]
const ENCODED: usize = KIND.encoded(5);

/// How long a ticket works: the time one transfer may take.
pub const LIFETIME: Duration = crate::workload::replica::TRANSFER_WINDOW;

/// One transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer {
    /// The volume.
    pub volume: Uuid,
    /// The node sending: the volume's.
    pub from: Uuid,
    /// The node receiving: a replica's.
    pub to: Uuid,
    /// The snapshot the stream carries.
    pub snapshot: Uuid,
    /// The snapshot it is an increment from, which the receiver holds; `None` for a whole one.
    pub parent: Option<Uuid>,
    /// When it stops working.
    pub expires_at: SystemTime,
}

/// The ticket for `t`, signed by `key`.
#[must_use]
pub fn mint(key: &SigningKey, t: &Transfer) -> String {
    let ids = [t.volume, t.from, t.to, t.snapshot, t.parent.unwrap_or_default()];
    KIND.mint(key, ids, t.expires_at)
}

/// The transfer `presented` names, if it is a ticket signed by a key in `keys` and not expired
/// at `now`.
pub fn open(keys: &KeyRing, presented: &str, now: SystemTime) -> Result<Transfer, SealError> {
    let ([volume, from, to, snapshot, parent], expires_at) = KIND.open(keys, presented, now)?;
    let parent = (!parent.is_nil()).then_some(parent);
    Ok(Transfer { volume, from, to, snapshot, parent, expires_at })
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

    fn transfer(now: SystemTime, parent: Option<Uuid>) -> Transfer {
        Transfer {
            volume: Uuid::now_v7(),
            from: Uuid::now_v7(),
            to: Uuid::now_v7(),
            snapshot: Uuid::now_v7(),
            parent,
            expires_at: now + LIFETIME,
        }
    }

    #[test]
    fn a_ticket_opens_as_minted_until_it_expires_and_never_under_another_key() {
        let (key, other) = (SigningKey::generate(3), SigningKey::generate(4));
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        for parent in [None, Some(Uuid::now_v7())] {
            let t = transfer(now, parent);
            let ticket = mint(&key, &t);
            assert!(ticket.len() <= ENCODED, "{}", ticket.len());
            assert_eq!(open(&ring(&key), &ticket, now), Ok(t));
            assert_eq!(open(&ring(&key), &ticket, t.expires_at), Err(SealError::Expired));
            assert_eq!(open(&ring(&other), &ticket, now), Err(SealError::Signature));
        }
    }

    #[test]
    fn a_ticket_altered_or_of_another_kind_is_refused() {
        let key = SigningKey::generate(3);
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ticket = mint(&key, &transfer(now, None));
        let mut altered = ticket.clone().into_bytes();
        if let Some(b) = altered.get_mut(PREFIX.len() + 10) {
            *b = if *b == b'A' { b'B' } else { b'A' };
        }
        let altered = String::from_utf8(altered).unwrap_or_default();
        assert!(open(&ring(&key), &altered, now).is_err());
        let cookie = super::super::device::mint(
            &key,
            &super::super::device::DeviceClaims {
                account: Uuid::now_v7(),
                device: Uuid::now_v7(),
                expires_at: now + LIFETIME,
            },
        );
        assert_eq!(open(&ring(&key), &cookie, now), Err(SealError::Malformed));
    }
}
