// SPDX-License-Identifier: AGPL-3.0-or-later

//! A ticket of identifiers: a fixed number of them and an expiry, signed by the control plane's
//! token key over a purpose label of the ticket's kind, so nothing else the key signs is taken
//! for one, and one kind is never taken for another. A transfer's ticket (§2.2 of
//! docs/06-storage.md) and a migration pass's (§8.1 of docs/05-scheduling.md) are both this.
//!
//! The form: a prefix of the kind's own, then the payload and its Ed25519 signature, each
//! base64url without padding, joined by a dot. The payload is a version byte, the key id, the
//! identifiers in order, the nil identifier standing for one that is absent, and the expiry in
//! seconds since the epoch, big-endian.

use super::sealed::SealError;
use super::token::{KeyRing, SigningKey};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer as _, VerifyingKey};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// The payload's version.
const VERSION: u8 = 1;

/// A kind of ticket: its prefix and the label its signature is over.
#[derive(Debug, Clone, Copy)]
pub(super) struct Kind {
    /// Every ticket of the kind starts with this.
    pub(super) prefix: &'static str,
    /// What the signature is over, before the payload.
    pub(super) purpose: &'static [u8],
}

impl Kind {
    /// The payload's length for `n` identifiers: version, key id, identifiers, expiry.
    const fn payload(n: usize) -> usize {
        1 + 4 + n * 16 + 8
    }

    /// The longest ticket of `n` identifiers: the prefix, the payload and a 64-byte signature
    /// in base64url, and the dot between them.
    pub(super) const fn encoded(self, n: usize) -> usize {
        self.prefix.len() + Self::payload(n).div_ceil(3) * 4 + 1 + 86
    }

    fn signed(self, payload: &[u8]) -> Vec<u8> {
        let mut message = self.purpose.to_vec();
        message.extend_from_slice(payload);
        message
    }

    /// The ticket of `ids`, expiring at `expires_at`, signed by `key`.
    pub(super) fn mint<const N: usize>(
        self,
        key: &SigningKey,
        ids: [Uuid; N],
        expires_at: SystemTime,
    ) -> String {
        let seconds = expires_at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let mut payload = Vec::with_capacity(Self::payload(N));
        payload.push(VERSION);
        payload.extend_from_slice(&key.key_id.to_be_bytes());
        for id in ids {
            payload.extend_from_slice(id.as_bytes());
        }
        payload.extend_from_slice(&seconds.to_be_bytes());
        let signing = ed25519_dalek::SigningKey::from_bytes(&key.seed());
        let signature = signing.sign(&self.signed(&payload));
        format!(
            "{}{}.{}",
            self.prefix,
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }

    /// The identifiers and expiry `presented` carries, if it is a ticket of this kind signed by
    /// a key in `keys` and not expired at `now`.
    pub(super) fn open<const N: usize>(
        self,
        keys: &KeyRing,
        presented: &str,
        now: SystemTime,
    ) -> Result<([Uuid; N], SystemTime), SealError> {
        if presented.len() > self.encoded(N) {
            return Err(SealError::Malformed);
        }
        let body = presented.strip_prefix(self.prefix).ok_or(SealError::Malformed)?;
        let (payload, signature) = body.split_once('.').ok_or(SealError::Malformed)?;
        let payload = URL_SAFE_NO_PAD.decode(payload).map_err(|_| SealError::Malformed)?;
        let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| SealError::Malformed)?;
        let (key_id, ids, expires_at) = parse::<N>(&payload)?;
        let public = keys.public_bytes(key_id).ok_or(SealError::Signature)?;
        let public = <[u8; 32]>::try_from(public.as_slice()).map_err(|_| SealError::Signature)?;
        let verifying = VerifyingKey::from_bytes(&public).map_err(|_| SealError::Signature)?;
        let signature = Signature::from_slice(&signature).map_err(|_| SealError::Signature)?;
        let message = self.signed(&payload);
        verifying.verify_strict(&message, &signature).map_err(|_| SealError::Signature)?;
        if now >= expires_at {
            return Err(SealError::Expired);
        }
        Ok((ids, expires_at))
    }
}

fn parse<const N: usize>(payload: &[u8]) -> Result<(u32, [Uuid; N], SystemTime), SealError> {
    if payload.len() != Kind::payload(N) {
        return Err(SealError::Malformed);
    }
    let (&[version], rest) = payload.split_first_chunk::<1>().ok_or(SealError::Malformed)?;
    if version != VERSION {
        return Err(SealError::Malformed);
    }
    let (key_id, mut rest) = rest.split_first_chunk::<4>().ok_or(SealError::Malformed)?;
    let mut ids = [Uuid::nil(); N];
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
    Ok((u32::from_be_bytes(*key_id), ids, expires_at))
}
