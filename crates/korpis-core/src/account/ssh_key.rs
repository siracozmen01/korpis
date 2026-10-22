// SPDX-License-Identifier: AGPL-3.0-or-later

//! The SSH public keys a person registers to reach their files over SFTP (§9.9 of
//! docs/08-identity.md): read from the line `ssh-keygen` writes, checked to be a kind and size
//! every current client makes and none weaker than 128 bits, and named by the SHA-256
//! fingerprint `ssh-keygen -l` prints. Only the public half is ever seen, and it is parsed here,
//! field by field from RFC 4253's wire form, so nothing about it is left to a later reader. Its
//! holder proves holding the private half by a signature ([`possession`]).

mod possession;
#[cfg(any(test, feature = "testing"))]
pub mod signing;

pub use possession::{NAMESPACE, possession_message};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use sha2::{Digest, Sha256};

/// The longest line read, in bytes: an RSA key of 8,192 bits with a comment fits.
pub const MAX_LINE: usize = 4 << 10;
/// The fewest bits an RSA key has: 128 bits of security (`research/evidence.md` §5).
pub const MIN_RSA_BITS: usize = 3_072;
/// The most bits an RSA key has, so that checking one costs a bounded amount: the most `ring`
/// verifies a signature of, which its holder proves holding it with.
pub const MAX_RSA_BITS: usize = 8_192;

/// What a key is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshKeyKind {
    /// Ed25519.
    Ed25519,
    /// ECDSA over NIST P-256.
    EcdsaP256,
    /// ECDSA over NIST P-384.
    EcdsaP384,
    /// RSA, of this many bits.
    Rsa(usize),
}

impl SshKeyKind {
    /// Its name on the wire and in an OpenSSH line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ed25519 => "ssh-ed25519",
            Self::EcdsaP256 => "ecdsa-sha2-nistp256",
            Self::EcdsaP384 => "ecdsa-sha2-nistp384",
            Self::Rsa(_) => "ssh-rsa",
        }
    }
}

/// A public key, checked: its kind and its wire form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshPublicKey {
    kind: SshKeyKind,
    blob: Vec<u8>,
}

/// Why a key is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SshKeyError(String);

fn refuse(message: impl Into<String>) -> SshKeyError {
    SshKeyError(message.into())
}

/// A reader of RFC 4253's length-prefixed fields.
struct Wire<'a>(&'a [u8]);

impl<'a> Wire<'a> {
    fn string(&mut self) -> Result<&'a [u8], SshKeyError> {
        let malformed = || refuse("the key is not a public key in SSH's wire form");
        let (len, rest) = self.0.split_first_chunk::<4>().ok_or_else(malformed)?;
        let len = usize::try_from(u32::from_be_bytes(*len)).map_err(|_| malformed())?;
        let (field, rest) = rest.split_at_checked(len).ok_or_else(malformed)?;
        self.0 = rest;
        Ok(field)
    }

    fn end(&self) -> Result<(), SshKeyError> {
        if self.0.is_empty() { Ok(()) } else { Err(refuse("the key has bytes after its fields")) }
    }
}

/// The bits of a positive `mpint`, refusing a negative one.
fn mpint_bits(m: &[u8]) -> Result<usize, SshKeyError> {
    if m.first().is_some_and(|b| b & 0x80 != 0) {
        return Err(refuse("an RSA field is negative"));
    }
    let trimmed = m.iter().position(|b| *b != 0).map_or(&[][..], |i| m.get(i..).unwrap_or(&[]));
    let Some(top) = trimmed.first() else { return Ok(0) };
    let top_bits = usize::try_from(u8::BITS - top.leading_zeros()).unwrap_or(8);
    Ok((trimmed.len() - 1) * 8 + top_bits)
}

fn kind_of(blob: &[u8]) -> Result<SshKeyKind, SshKeyError> {
    let mut w = Wire(blob);
    let name = w.string()?;
    let kind = match name {
        b"ssh-ed25519" => {
            if w.string()?.len() != 32 {
                return Err(refuse("an Ed25519 key is 32 bytes"));
            }
            SshKeyKind::Ed25519
        }
        b"ecdsa-sha2-nistp256" | b"ecdsa-sha2-nistp384" => {
            let (curve, len, kind) = if name == b"ecdsa-sha2-nistp256" {
                (&b"nistp256"[..], 65, SshKeyKind::EcdsaP256)
            } else {
                (&b"nistp384"[..], 97, SshKeyKind::EcdsaP384)
            };
            let point = (w.string()?, w.string()?);
            if point.0 != curve || point.1.len() != len || point.1.first() != Some(&4) {
                return Err(refuse("an ECDSA key is an uncompressed point on its named curve"));
            }
            kind
        }
        b"ssh-rsa" => {
            let (e, n) = (w.string()?, w.string()?);
            let bits = mpint_bits(n)?;
            if !(MIN_RSA_BITS..=MAX_RSA_BITS).contains(&bits) {
                let message = format!("an RSA key is {MIN_RSA_BITS} to {MAX_RSA_BITS} bits");
                return Err(refuse(format!("{message}; this one is {bits}")));
            }
            if e.last().is_none_or(|b| b & 1 == 0) || mpint_bits(e)? < 2 {
                return Err(refuse("an RSA key's exponent is odd and at least 3"));
            }
            SshKeyKind::Rsa(bits)
        }
        _ => {
            return Err(refuse("a key is Ed25519, ECDSA over P-256 or P-384, or RSA"));
        }
    };
    w.end()?;
    Ok(kind)
}

impl SshPublicKey {
    /// The key a line `ssh-keygen` writes names, `KIND BASE64 [COMMENT]`, its comment dropped.
    ///
    /// # Errors
    /// The line is too long or not of that form, or the key is not a kind and size taken.
    pub fn parse(line: &str) -> Result<Self, SshKeyError> {
        if line.len() > MAX_LINE {
            return Err(refuse(format!("a key's line is at most {} KiB", MAX_LINE >> 10)));
        }
        let mut parts = line.split_ascii_whitespace();
        let (Some(name), Some(encoded)) = (parts.next(), parts.next()) else {
            return Err(refuse("a public key's line is its kind, then the key in base64"));
        };
        let blob = STANDARD.decode(encoded).map_err(|_| refuse("the key is not base64"))?;
        let key = Self::from_blob(blob)?;
        if key.kind.name() != name {
            return Err(refuse("the line names another kind than its key is"));
        }
        Ok(key)
    }

    /// The key whose wire form is `blob`.
    ///
    /// # Errors
    /// It is not a kind and size taken.
    pub fn from_blob(blob: Vec<u8>) -> Result<Self, SshKeyError> {
        Ok(Self { kind: kind_of(&blob)?, blob })
    }

    /// What it is.
    #[must_use]
    pub const fn kind(&self) -> SshKeyKind {
        self.kind
    }

    /// Its wire form.
    #[must_use]
    pub fn blob(&self) -> &[u8] {
        &self.blob
    }

    /// Its fingerprint as `ssh-keygen -l` prints it: `SHA256:` and the digest of its wire form,
    /// in base64 without padding.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&self.blob)))
    }

    /// The key as a line of `authorized_keys`, without a comment.
    #[must_use]
    pub fn openssh(&self) -> String {
        format!("{} {}", self.kind.name(), STANDARD.encode(&self.blob))
    }
}

/// A key as an account holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshKey {
    /// Its identifier.
    pub id: uuid::Uuid,
    /// The account it reaches files as.
    pub account: uuid::Uuid,
    /// The key.
    pub key: SshPublicKey,
    /// What its holder calls it.
    pub label: String,
    /// The organization the session that registered it was confined beneath, if it was; what
    /// it opens is confined there too (§9.8 and §9.9 of docs/08-identity.md).
    pub within: Option<uuid::Uuid>,
    /// When it was added.
    pub added_at: std::time::SystemTime,
    /// When a file session was last authorized with it.
    pub last_used_at: Option<std::time::SystemTime>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    // Made with ssh-keygen 2026-10-22; each fingerprint is what `ssh-keygen -lf -E sha256` printed.
    const ED25519: &str = "ssh-ed25519 \
        AAAAC3NzaC1lZDI1NTE5AAAAIHYUMOAa8CAodE24mCEXvC+hwFHXgRkuGMAE8eBMqcsx ada@laptop";
    const ECDSA_384: &str = "ecdsa-sha2-nistp384 \
        AAAAE2VjZHNhLXNoYTItbmlzdHAzODQAAAAIbmlzdHAzODQAAABhBI1C2vmSz94ZqVcpFJZUhqAKMkVKj/mfaw3j\
        nTfpKoaH29xeSCc2IgpMn1FiTcxNU2LWTOFDietfI9pT5mM62lEd7IxYMkpAl5vv79Wh75LJZkd0hRvT0A0lTfQ6\
        MO+8Tw== x";
    const RSA_2048: &str = "ssh-rsa \
        AAAAB3NzaC1yc2EAAAADAQABAAABAQCmA8kPHkrr+ucw5DrC9rSBi/+LbyMwwn2mQzGq51Lpkokwq9Rt4d8S2sdV\
        vOThXVzI3M/Hf6HxhLfkXTYNRHeTsgzwM9eiKv+BZLfjlmAXaJqgf6SJrOyfdWlHIRSXR1U9CwVChAKEki+LL9TF\
        X361dGHolEz0nAepDH+unhO1xppDTiAnGurEJryalkG5lagaMZcV6p6sNPgcIDXeG7pnWz49Lx+pNJWRjkg/DiwI\
        kyOy9FR/WR2yZ3IXjlC+sAPuN0YxFS/DCwiq4vYx09Fl0+tN+t+nfqV8vvugKB6JvO9463uOzCCLMveel7TTcmbe\
        uenCktgYOF19wuPq7XwJ small";
    const RSA_3072: &str = "ssh-rsa \
        AAAAB3NzaC1yc2EAAAADAQABAAABgQC2h/6n0G/m7+nZZn4GpeznbzDDlCKJGEi28DxoB1ZhEEAdNUHha8870NGE\
        oAz3Gwusaq134tIYJ7zSruh91Qffa0mC1VA/3rAR/fqT35z19BdAwmlDiugLC/jcmyoXXhDudvJFpLjhtCKvLAFI\
        F10UnescB6lAC/8lUXFogeUvlXFQHcTH9TGVJn37V0N99wrAXSJLicHq6Wway2ZKDqDPz0Y8visM68+uTvxz18N6\
        dWm8h5/bTYN1q7hCWeEsc+Oer/2Ic8jJR1sB8qlSLjERvPFUJSlO2ddNHjmUxOdz158g+2cqjs1VjZcZM370jpG2\
        9vb2T4ffIxiW/L0eZZHHKbWrLYGSXuGlRvM8YVEdrurACknIy/N6/Mp1fPHaNUt8R9O9Y2XDcFPqkW9YEfjSW5G6\
        RCUgyy0kMXaxcxsyL7xZwIOTdW29/lbln+r57zWQPQpc1mLQQBxqe8owNDzja+AH4KzUhC9PrKlxSt+P9ElSfuGd\
        +XANyVAcgDjOweE= big";

    #[test]
    fn a_key_is_named_by_the_fingerprint_ssh_keygen_prints() {
        let cases = [
            (ED25519, SshKeyKind::Ed25519, "SHA256:I3XZHy63BiL9AKC20atpgIFioqsBqGVMs7EMP2trtmQ"),
            (
                ECDSA_384,
                SshKeyKind::EcdsaP384,
                "SHA256:yx0WiXDI8eba1WqAhU15z8R6zvjKzVanh+pzi9rEHMg",
            ),
            (RSA_3072, SshKeyKind::Rsa(3072), "SHA256:X16+UiMGdPgtAOw8QTFBUI/irkJPjmjVEextPO0XloE"),
        ];
        for (line, kind, fingerprint) in cases {
            let key = SshPublicKey::parse(line).unwrap();
            assert_eq!((key.kind(), key.fingerprint().as_str()), (kind, fingerprint));
            let again = SshPublicKey::parse(&key.openssh()).unwrap();
            assert_eq!(again, key, "its line, comment dropped, is the same key");
        }
    }

    #[test]
    fn a_weak_short_or_mislabelled_key_is_refused() {
        let small = SshPublicKey::parse(RSA_2048).unwrap_err();
        assert!(small.to_string().contains("this one is 2048"), "{small}");
        let relabelled = ED25519.replacen("ssh-ed25519", "ssh-rsa", 1);
        assert!(SshPublicKey::parse(&relabelled).is_err(), "the line's kind is the key's");
        for bad in ["", "ssh-ed25519", "ssh-ed25519 !!!", "ssh-dss AAAAB3NzaC1kc3M="] {
            assert!(SshPublicKey::parse(bad).is_err(), "{bad:?}");
        }
        let key = SshPublicKey::parse(ED25519).unwrap();
        let mut longer = key.blob().to_vec();
        longer.push(0);
        assert!(SshPublicKey::from_blob(longer).is_err(), "nothing after its fields");
        let mut cut = key.blob().to_vec();
        cut.truncate(cut.len() - 1);
        assert!(SshPublicKey::from_blob(cut).is_err(), "nothing missing");
    }
}
