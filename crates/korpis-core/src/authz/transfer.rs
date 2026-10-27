// SPDX-License-Identifier: AGPL-3.0-or-later

//! Transfer tickets (§2.2 of docs/06-storage.md): what lets one node write into another node's
//! replica of a volume. The control plane puts one in the sending node's assignment and the
//! receiving node's, and the receiving agent checks it offline with the token keys it holds,
//! so a transfer under way survives the control plane's absence until the ticket expires.
//!
//! A ticket names one transfer: the volume, the node sending, the node receiving, the snapshot
//! the stream carries, the one it is an increment from (none for a whole one), and its expiry. It
//! is signed by the control plane's token key, as the claims of [`super::sealed`] are, over a
//! purpose label of its own, so nothing else the key signs is taken for one, and a ticket is
//! taken for nothing else: it carries no grant and no right.
//!
//! The form: `korpistt_`, then the payload and its Ed25519 signature, each base64url without
//! padding, joined by a dot. The payload is a version byte, the key id, the five identifiers,
//! the parent the nil identifier for a whole snapshot, and the expiry in seconds since the epoch,
//! big-endian.

use super::token::{KeyRing, SigningKey};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer as _, VerifyingKey};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub use super::sealed::SealError as TicketError;
use TicketError as SealError;

/// Every ticket starts with this.
pub const PREFIX: &str = "korpistt_";
/// What the signature is over, before the payload.
const PURPOSE: &[u8] = b"korpis.transfer.v1\0";
/// The payload's version.
const VERSION: u8 = 1;
/// The payload: version, key id, five identifiers, expiry.
const PAYLOAD: usize = 1 + 4 + 5 * 16 + 8;
/// The payload and a 64-byte signature in base64url, the dot between them, and the prefix.
const ENCODED: usize = PREFIX.len() + 124 + 1 + 86;

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
    let seconds = t.expires_at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut payload = Vec::with_capacity(PAYLOAD);
    payload.push(VERSION);
    payload.extend_from_slice(&key.key_id.to_be_bytes());
    for id in [t.volume, t.from, t.to, t.snapshot, t.parent.unwrap_or_default()] {
        payload.extend_from_slice(id.as_bytes());
    }
    payload.extend_from_slice(&seconds.to_be_bytes());
    let signing = ed25519_dalek::SigningKey::from_bytes(&key.seed());
    let signature = signing.sign(&signed(&payload));
    format!(
        "{PREFIX}{}.{}",
        URL_SAFE_NO_PAD.encode(&payload),
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

/// The transfer `presented` names, if it is a ticket signed by a key in `keys` and not expired
/// at `now`.
pub fn open(keys: &KeyRing, presented: &str, now: SystemTime) -> Result<Transfer, SealError> {
    if presented.len() > ENCODED {
        return Err(SealError::Malformed);
    }
    let body = presented.strip_prefix(PREFIX).ok_or(SealError::Malformed)?;
    let (payload, signature) = body.split_once('.').ok_or(SealError::Malformed)?;
    let payload = URL_SAFE_NO_PAD.decode(payload).map_err(|_| SealError::Malformed)?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| SealError::Malformed)?;
    let (key_id, transfer) = parse(&payload)?;
    let public = keys.public_bytes(key_id).ok_or(SealError::Signature)?;
    let public = <[u8; 32]>::try_from(public.as_slice()).map_err(|_| SealError::Signature)?;
    let verifying = VerifyingKey::from_bytes(&public).map_err(|_| SealError::Signature)?;
    let signature = Signature::from_slice(&signature).map_err(|_| SealError::Signature)?;
    verifying.verify_strict(&signed(&payload), &signature).map_err(|_| SealError::Signature)?;
    if now >= transfer.expires_at {
        return Err(SealError::Expired);
    }
    Ok(transfer)
}

fn parse(payload: &[u8]) -> Result<(u32, Transfer), SealError> {
    if payload.len() != PAYLOAD {
        return Err(SealError::Malformed);
    }
    let (&[version], rest) = payload.split_first_chunk::<1>().ok_or(SealError::Malformed)?;
    if version != VERSION {
        return Err(SealError::Malformed);
    }
    let (key_id, mut rest) = rest.split_first_chunk::<4>().ok_or(SealError::Malformed)?;
    let mut ids = [Uuid::nil(); 5];
    for id in &mut ids {
        let (bytes, tail) = rest.split_first_chunk::<16>().ok_or(SealError::Malformed)?;
        *id = Uuid::from_bytes(*bytes);
        rest = tail;
    }
    let (expiry, _) = rest.split_first_chunk::<8>().ok_or(SealError::Malformed)?;
    // Read before the signature is checked, so it is anyone's bytes: an expiry past what a time
    // can hold is malformed, never an overflow.
    let expires_at = UNIX_EPOCH
        .checked_add(Duration::from_secs(u64::from_be_bytes(*expiry)))
        .ok_or(SealError::Malformed)?;
    let [volume, from, to, snapshot, parent] = ids;
    let parent = (!parent.is_nil()).then_some(parent);
    let transfer = Transfer { volume, from, to, snapshot, parent, expires_at };
    Ok((u32::from_be_bytes(*key_id), transfer))
}

fn signed(payload: &[u8]) -> Vec<u8> {
    let mut message = PURPOSE.to_vec();
    message.extend_from_slice(payload);
    message
}

#[cfg(test)]
mod tests {
    use super::*;

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
