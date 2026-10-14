// SPDX-License-Identifier: Apache-2.0

//! Keys sealed to one recipient (§5.5 of docs/06-storage.md). A key management provider never
//! answers with a data key in the clear: every call names a recipient's one-time X25519 public
//! key, and the provider seals the key to it with HPKE (RFC 9180, base mode, DHKEM(X25519,
//! HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305), bound to the call's context. What the control
//! plane stores and passes on is then opened only by the recipient: a backup job on its node, or
//! the control plane's own read, whose secret lives in memory for one call.
//!
//! ```text
//! sealed = encapsulated key (32 bytes) || ciphertext and tag of the 32-byte key
//! ```

use hpke::aead::ChaCha20Poly1305;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable as _, Kem as _, OpModeR, OpModeS, Serializable as _};

/// A data key's length.
pub const KEY_LEN: usize = 32;

/// A recipient public key's length.
pub const PUBLIC_LEN: usize = 32;

/// What the sealing is for, as HPKE's info.
const INFO: &[u8] = b"korpis.key.v1";

type Kem = X25519HkdfSha256;

/// Why a key could not be sealed or opened.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SealError {
    /// The recipient's public key is not one.
    #[error("not an X25519 public key")]
    Recipient,
    /// The system's random source failed.
    #[error("no randomness")]
    Random,
    /// What was to be sealed is not a key.
    #[error("not a {KEY_LEN}-byte key")]
    Key,
    /// It was not sealed to this recipient, for this context, or it was changed.
    #[error("the sealed key did not open")]
    Open,
}

/// A one-time recipient: its secret never leaves the process that made it.
pub struct Recipient {
    secret: <Kem as hpke::Kem>::PrivateKey,
    public: [u8; PUBLIC_LEN],
}

impl std::fmt::Debug for Recipient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recipient").field("public", &self.public).finish_non_exhaustive()
    }
}

impl Recipient {
    /// A new recipient, from the system's random source.
    ///
    /// # Errors
    /// The random source failed.
    pub fn new() -> Result<Self, SealError> {
        let mut ikm = [0u8; 32];
        getrandom::fill(&mut ikm).map_err(|_| SealError::Random)?;
        let (secret, public) = Kem::derive_keypair(&ikm);
        ikm.fill(0);
        let public: [u8; PUBLIC_LEN] =
            public.to_bytes().as_slice().try_into().map_err(|_| SealError::Recipient)?;
        Ok(Self { secret, public })
    }

    /// Its public key, which a provider seals to.
    #[must_use]
    pub const fn public(&self) -> &[u8; PUBLIC_LEN] {
        &self.public
    }

    /// Opens `sealed`, which was sealed to this recipient for `context`.
    ///
    /// # Errors
    /// [`SealError::Open`] when it was not, or was changed.
    pub fn open(&self, sealed: &[u8], context: &[u8]) -> Result<[u8; KEY_LEN], SealError> {
        if sealed.len() < PUBLIC_LEN {
            return Err(SealError::Open);
        }
        let (encapped, ciphertext) = sealed.split_at(PUBLIC_LEN);
        let encapped =
            <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).map_err(|_| SealError::Open)?;
        let mode = OpModeR::Base;
        let opened = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
            &mode,
            &self.secret,
            &encapped,
            INFO,
            ciphertext,
            context,
        )
        .map_err(|_| SealError::Open)?;
        opened.as_slice().try_into().map_err(|_| SealError::Open)
    }
}

/// Seals `key` to the recipient whose public key is `recipient`, for `context`.
///
/// # Errors
/// The recipient is not an X25519 public key, or `key` is not a key.
pub fn seal(recipient: &[u8], key: &[u8], context: &[u8]) -> Result<Vec<u8>, SealError> {
    if key.len() != KEY_LEN {
        return Err(SealError::Key);
    }
    let public =
        <Kem as hpke::Kem>::PublicKey::from_bytes(recipient).map_err(|_| SealError::Recipient)?;
    let (encapped, ciphertext) = hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem>(
        &OpModeS::Base,
        &public,
        INFO,
        key,
        context,
    )
    .map_err(|_| SealError::Recipient)?;
    let mut sealed = encapped.to_bytes().to_vec();
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_opens_only_for_its_recipient_and_its_context() {
        let r = Recipient::new().unwrap();
        let key = [9u8; KEY_LEN];
        let sealed = seal(r.public(), &key, b"rep_1/wl_1").unwrap();
        assert_eq!(r.open(&sealed, b"rep_1/wl_1").unwrap(), key);
        assert_eq!(r.open(&sealed, b"rep_1/wl_2"), Err(SealError::Open), "another context");
        let other = Recipient::new().unwrap();
        assert_eq!(other.open(&sealed, b"rep_1/wl_1"), Err(SealError::Open), "another recipient");
        let mut changed = sealed.clone();
        changed[40] ^= 1;
        assert_eq!(r.open(&changed, b"rep_1/wl_1"), Err(SealError::Open));
        assert_eq!(seal(r.public(), &[1; 16], b""), Err(SealError::Key));
        assert_eq!(seal(&[0; 3], &key, b""), Err(SealError::Recipient));
    }
}
