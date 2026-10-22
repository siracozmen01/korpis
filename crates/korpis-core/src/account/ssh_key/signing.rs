// SPDX-License-Identifier: AGPL-3.0-or-later

//! An SSH key and its proof of possession made in process, for tests: an Ed25519 key from a seed,
//! its line, and its signature over an account's name as `ssh-keygen -Y sign` makes one, so a
//! test registers a key without a program to sign with.

use super::possession::{NAMESPACE, possession_message};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::digest;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

fn string(out: &mut Vec<u8>, field: &[u8]) {
    out.extend_from_slice(&u32::try_from(field.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(field);
}

/// The key `seed` makes, as a line `ssh-keygen` writes, and its signature proving its holder adds
/// it to `account`, `usr_…`.
///
/// # Panics
/// Never: every seed makes a key.
#[must_use]
#[allow(clippy::missing_panics_doc, reason = "documented above")]
pub fn ed25519_possession(seed: [u8; 32], account: &str) -> (String, String) {
    #[allow(clippy::expect_used, reason = "every 32-byte seed is a key")]
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed).expect("a key");
    let mut blob = Vec::new();
    string(&mut blob, b"ssh-ed25519");
    string(&mut blob, pair.public_key().as_ref());
    let hashed = digest::digest(&digest::SHA512, possession_message(account).as_bytes());
    let mut signed = b"SSHSIG".to_vec();
    for f in [NAMESPACE.as_bytes(), b"", b"sha512", hashed.as_ref()] {
        string(&mut signed, f);
    }
    let mut sig = Vec::new();
    string(&mut sig, b"ssh-ed25519");
    string(&mut sig, pair.sign(&signed).as_ref());
    let mut whole = b"SSHSIG".to_vec();
    whole.extend_from_slice(&1u32.to_be_bytes());
    for f in [&blob[..], NAMESPACE.as_bytes(), b"", b"sha512", &sig] {
        string(&mut whole, f);
    }
    let encoded = STANDARD.encode(&whole);
    let lines: Vec<&str> =
        encoded.as_bytes().chunks(70).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect();
    let armoured = format!(
        "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
        lines.join("\n")
    );
    (format!("ssh-ed25519 {} test", STANDARD.encode(&blob)), armoured)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::super::SshPublicKey;
    use super::*;

    #[test]
    fn a_key_made_here_proves_its_possession_as_one_ssh_keygen_signed() {
        let (line, sig) = ed25519_possession([7; 32], "usr_1");
        let key = SshPublicKey::parse(&line).unwrap();
        assert_eq!(key.check_possession(&sig, "usr_1"), Ok(()));
        assert!(key.check_possession(&sig, "usr_2").is_err());
    }
}
