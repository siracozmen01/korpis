// SPDX-License-Identifier: AGPL-3.0-or-later

//! Proof that whoever registers an SSH key holds its private half (§9.9 of docs/08-identity.md):
//! an SSH signature as `ssh-keygen -Y sign` makes one (OpenSSH's `PROTOCOL.sshsig`), in Korpis's
//! namespace, over the text naming the account the key is registered to. A public key is public,
//! so naming one proves nothing; a signature over the account's name is good for that account
//! alone, so a key cannot be registered to anyone but by its holder. Verified with `ring`, as a
//! passkey's assertion is, and an RSA signature only with SHA-2.

use super::{SshKeyError, SshKeyKind, SshPublicKey, Wire, refuse};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::{digest, signature};

/// The namespace `ssh-keygen -Y sign -n` is given.
pub const NAMESPACE: &str = "korpis-ssh-key";

const BEGIN: &str = "-----BEGIN SSH SIGNATURE-----";
const END: &str = "-----END SSH SIGNATURE-----";
/// The longest armoured signature read: an RSA key of 8,192 bits and its signature fit.
const MAX_ARMOURED: usize = 8 << 10;

/// What a key's holder signs to register it to `account`, named as `usr_…`.
#[must_use]
pub fn possession_message(account: &str) -> String {
    format!("korpis ssh-key add {account}")
}

fn malformed() -> SshKeyError {
    refuse("the signature is not one `ssh-keygen -Y sign` makes")
}

/// The bytes between the armour's lines.
fn unarmoured(armoured: &str) -> Result<Vec<u8>, SshKeyError> {
    if armoured.len() > MAX_ARMOURED {
        return Err(refuse(format!("a signature is at most {} KiB", MAX_ARMOURED >> 10)));
    }
    let body = armoured.trim().strip_prefix(BEGIN).and_then(|b| b.strip_suffix(END));
    let body: String = body.ok_or_else(malformed)?.split_ascii_whitespace().collect();
    STANDARD.decode(body).map_err(|_| malformed())
}

/// A field read where a signature is, its errors saying so.
fn field<'a>(w: &mut Wire<'a>) -> Result<&'a [u8], SshKeyError> {
    w.string().map_err(|_| malformed())
}

/// The fixed-width big-endian form of the positive `mpint` `m`, `width` bytes.
fn fixed(m: &[u8], width: usize) -> Result<Vec<u8>, SshKeyError> {
    let start = m.iter().position(|b| *b != 0).unwrap_or(m.len());
    let digits = m.get(start..).unwrap_or_default();
    let pad = width.checked_sub(digits.len()).ok_or_else(malformed)?;
    Ok([vec![0; pad], digits.to_vec()].concat())
}

/// `m` without the leading zeros an `mpint` carries for its sign.
fn unsigned(m: &[u8]) -> &[u8] {
    let start = m.iter().position(|b| *b != 0).unwrap_or(m.len());
    m.get(start..).unwrap_or_default()
}

impl SshPublicKey {
    /// Checks that `armoured`, as `ssh-keygen -Y sign -n korpis-ssh-key` writes it, is this key's
    /// signature over [`possession_message`] for `account`.
    ///
    /// # Errors
    /// It is not such a signature: of another key, namespace, or text, or not one at all.
    pub fn check_possession(&self, armoured: &str, account: &str) -> Result<(), SshKeyError> {
        let blob = unarmoured(armoured)?;
        let rest = blob.strip_prefix(b"SSHSIG").ok_or_else(malformed)?;
        let (version, rest) = rest.split_first_chunk::<4>().ok_or_else(malformed)?;
        if u32::from_be_bytes(*version) != 1 {
            return Err(malformed());
        }
        let mut w = Wire(rest);
        let (key, namespace, reserved) = (field(&mut w)?, field(&mut w)?, field(&mut w)?);
        let (hash, sig) = (field(&mut w)?, field(&mut w)?);
        w.end().map_err(|_| malformed())?;
        if key != self.blob() {
            return Err(refuse("the signature is another key's"));
        }
        if namespace != NAMESPACE.as_bytes() {
            return Err(refuse(format!("the signature is not in the namespace {NAMESPACE}")));
        }
        let algorithm = match hash {
            b"sha512" => &digest::SHA512,
            b"sha256" => &digest::SHA256,
            _ => return Err(malformed()),
        };
        let hashed = digest::digest(algorithm, possession_message(account).as_bytes());
        let mut signed = b"SSHSIG".to_vec();
        for f in [NAMESPACE.as_bytes(), reserved, hash, hashed.as_ref()] {
            signed.extend_from_slice(&u32::try_from(f.len()).unwrap_or(u32::MAX).to_be_bytes());
            signed.extend_from_slice(f);
        }
        self.verify(&signed, sig)
            .map_err(|_| refuse("the signature does not prove holding the key for this account"))
    }

    /// Checks `sig`, an SSH signature blob, over `signed` by this key.
    fn verify(&self, signed: &[u8], sig: &[u8]) -> Result<(), SshKeyError> {
        let mut s = Wire(sig);
        let (format, bytes) = (field(&mut s)?, field(&mut s)?);
        s.end().map_err(|_| malformed())?;
        let mut k = Wire(self.blob());
        let name = field(&mut k)?;
        let refused = |_| malformed();
        match (self.kind(), format) {
            (SshKeyKind::Ed25519, b"ssh-ed25519") => {
                let point = field(&mut k)?;
                signature::UnparsedPublicKey::new(&signature::ED25519, point)
                    .verify(signed, bytes)
                    .map_err(refused)
            }
            (SshKeyKind::EcdsaP256 | SshKeyKind::EcdsaP384, f) if f == name => {
                let (_, point) = (field(&mut k)?, field(&mut k)?);
                let (algorithm, width): (&signature::EcdsaVerificationAlgorithm, usize) =
                    if self.kind() == SshKeyKind::EcdsaP256 {
                        (&signature::ECDSA_P256_SHA256_FIXED, 32)
                    } else {
                        (&signature::ECDSA_P384_SHA384_FIXED, 48)
                    };
                let mut rs = Wire(bytes);
                let (r, s) = (field(&mut rs)?, field(&mut rs)?);
                let fixed_rs = [fixed(r, width)?, fixed(s, width)?].concat();
                signature::UnparsedPublicKey::new(algorithm, point)
                    .verify(signed, &fixed_rs)
                    .map_err(refused)
            }
            (SshKeyKind::Rsa(_), b"rsa-sha2-512" | b"rsa-sha2-256") => {
                let (e, n) = (field(&mut k)?, field(&mut k)?);
                let algorithm = if format == b"rsa-sha2-512" {
                    &signature::RSA_PKCS1_2048_8192_SHA512
                } else {
                    &signature::RSA_PKCS1_2048_8192_SHA256
                };
                let key = signature::RsaPublicKeyComponents { n: unsigned(n), e: unsigned(e) };
                key.verify(algorithm, signed, bytes).map_err(refused)
            }
            (SshKeyKind::Rsa(_), _) => Err(refuse("an RSA signature is made with SHA-2")),
            _ => Err(malformed()),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    const ACCOUNT: &str = "usr_0193b1c2d3e4f5a6b7c8d9e0f1a2b3c4";

    fn key(line: &str) -> SshPublicKey {
        SshPublicKey::parse(line).unwrap()
    }

    #[test]
    fn a_signature_by_the_key_over_the_account_proves_holding_it_for_every_kind() {
        let kinds = [
            (include_str!("testdata/ed.pub"), include_str!("testdata/ed.sig")),
            (include_str!("testdata/p256.pub"), include_str!("testdata/p256.sig")),
            (include_str!("testdata/p256.pub"), include_str!("testdata/p256-sha256.sig")),
            (include_str!("testdata/p384.pub"), include_str!("testdata/p384.sig")),
            (include_str!("testdata/rsa.pub"), include_str!("testdata/rsa.sig")),
        ];
        for (line, sig) in kinds {
            assert_eq!(key(line).check_possession(sig, ACCOUNT), Ok(()), "{line}");
        }
    }

    #[test]
    fn a_signature_proves_nothing_for_another_account_key_or_namespace() {
        let ed = key(include_str!("testdata/ed.pub"));
        let sig = include_str!("testdata/ed.sig");
        let other = "usr_0193b1c2d3e4f5a6b7c8d9e0f1a2b3c5";
        assert!(ed.check_possession(sig, other).is_err(), "another account");
        let p256 = key(include_str!("testdata/p256.pub"));
        assert!(p256.check_possession(sig, ACCOUNT).is_err(), "another key");
        let elsewhere = include_str!("testdata/ed-other-namespace.sig");
        assert!(ed.check_possession(elsewhere, ACCOUNT).is_err(), "another namespace");
        assert!(ed.check_possession("not a signature", ACCOUNT).is_err());
        let tampered = sig.replacen("Dz", "Dy", 1);
        assert!(ed.check_possession(&tampered, ACCOUNT).is_err(), "a byte changed");
    }
}
