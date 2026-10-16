// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in (§9.6 of docs/08-identity.md): the factors an account may hold besides a passkey,
//! which `webauthn` verifies. A password is never a sign-in alone; a TOTP code or a recovery code
//! is the second factor beside it.

pub mod password;
pub mod record;
pub mod recovery;
pub mod totp;

/// The operating system could not supply randomness.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no randomness available: {0}")]
pub struct EntropyError(pub String);

/// `N` bytes from the operating system's generator.
pub(crate) fn random<const N: usize>() -> Result<[u8; N], EntropyError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| EntropyError(e.to_string()))?;
    Ok(bytes)
}

/// 32 random bytes: a `WebAuthn` challenge, or a test credential's identifier.
pub fn nonce() -> Result<[u8; 32], EntropyError> {
    random()
}

/// RFC 4648 base32, upper case, without padding: what authenticator apps read a TOTP secret in,
/// and what a recovery code is written in.
pub(crate) fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let symbol = |index: u32| {
        let index = usize::try_from(index & 31).unwrap_or(0);
        ALPHABET.get(index).map_or('A', |b| char::from(*b))
    };
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(symbol(buffer >> bits));
        }
    }
    if bits > 0 {
        out.push(symbol(buffer << (5 - bits)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_is_rfc_4648s_without_padding() {
        // RFC 4648 §10.
        let vectors =
            [("", ""), ("f", "MY"), ("fo", "MZXQ"), ("foo", "MZXW6"), ("foobar", "MZXW6YTBOI")];
        for (plain, encoded) in vectors {
            assert_eq!(base32(plain.as_bytes()), encoded);
        }
    }
}
