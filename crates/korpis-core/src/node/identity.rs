// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's identity (§7 of docs/02-architecture.md): an Ed25519 key the node generates for
//! itself, whose public half the control plane records at enrollment. Every session's first
//! message proves the node holds the private half by signing the string [`hello_message`] builds,
//! and the control plane checks it with [`verify`]. The agent and the control plane share this
//! one definition, so they cannot disagree about what is signed.

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use std::fmt::Write as _;

/// The length of a public key, raw.
pub const PUBLIC_KEY_LEN: usize = 32;

/// What `NodeAuth.signature` signs: `korpis.agent.v1.Hello|<node>|<seconds>.<nanos>|<nonce>`,
/// with the timestamp as its Unix seconds and its nanoseconds in exactly nine digits, and the
/// nonce in lowercase hexadecimal.
#[must_use]
pub fn hello_message(node: &str, seconds: i64, nanos: u32, nonce: &[u8]) -> Vec<u8> {
    proof_message("korpis.agent.v1.Hello", node, seconds, nanos, nonce)
}

/// What a `DataHello`'s `NodeAuth.signature` signs: the same form as [`hello_message`] under
/// `korpis.agent.v1.DataHello`, so a relay that sees one cannot present it as a control-stream
/// Hello, followed by `|` and the URL of the relay the node dialled, so a relay cannot present
/// it to another relay either: relays share no nonce memory (§3.1 of docs/02-architecture.md).
/// The URL is last, after a nonce that holds no `|`, so whatever it contains is unambiguous.
#[must_use]
pub fn data_hello_message(
    (node, relay): (&str, &str),
    seconds: i64,
    nanos: u32,
    nonce: &[u8],
) -> Vec<u8> {
    let mut m = proof_message("korpis.agent.v1.DataHello", node, seconds, nanos, nonce);
    m.push(b'|');
    m.extend_from_slice(relay.as_bytes());
    m
}

/// What a relay signs to open its feed (§3.1 of docs/02-architecture.md): the same form as
/// [`hello_message`] under `korpis.relay.v1.Feed`, with the relay's identifier in place of the
/// node's, signed by the relay's identity key; followed by `|`, the SSH host key of the SFTP it
/// serves in lowercase hexadecimal, `|`, and the address clients reach it at, both empty where it
/// serves none, so the signature vouches for where clients are sent and which key they are told
/// to expect. The address is last, after a key that holds no `|`, so it is unambiguous.
#[must_use]
pub fn relay_feed_message(
    (relay, (host_key, address)): (&str, (&[u8], &str)),
    seconds: i64,
    nanos: u32,
    nonce: &[u8],
) -> Vec<u8> {
    let mut m = proof_message("korpis.relay.v1.Feed", relay, seconds, nanos, nonce);
    m.push(b'|');
    for b in host_key {
        // Writing to a Vec cannot fail.
        let _ = std::io::Write::write_fmt(&mut m, format_args!("{b:02x}"));
    }
    m.push(b'|');
    m.extend_from_slice(address.as_bytes());
    m
}

/// What a relay signs to renew its certificate (§3.1 of docs/02-architecture.md): the same form as
/// [`relay_feed_message`] under `korpis.relay.v1.Renew`, followed by `|` and the new TLS public
/// key in lowercase hexadecimal, so the signature vouches for the key the certificate will name.
#[must_use]
pub fn relay_renew_message(
    (relay, tls_key): (&str, &[u8]),
    seconds: i64,
    nanos: u32,
    nonce: &[u8],
) -> Vec<u8> {
    let mut m = proof_message("korpis.relay.v1.Renew", relay, seconds, nanos, nonce);
    m.push(b'|');
    for b in tls_key {
        // Writing to a Vec cannot fail.
        let _ = std::io::Write::write_fmt(&mut m, format_args!("{b:02x}"));
    }
    m
}

/// What a relay serving SFTP signs to ask for a file token (§9.9 of docs/08-identity.md): the
/// same form as [`relay_feed_message`] under `korpis.relay.v1.AuthorizeFiles`, followed by `|`
/// and the SHA-256 of what it asks for in lowercase hexadecimal, so the signature vouches for
/// that request alone and a captured one is good for no other.
#[must_use]
pub fn relay_files_message(
    (relay, asked): (&str, &[u8; 32]),
    seconds: i64,
    nanos: u32,
    nonce: &[u8],
) -> Vec<u8> {
    let mut m = proof_message("korpis.relay.v1.AuthorizeFiles", relay, seconds, nanos, nonce);
    m.push(b'|');
    for b in asked {
        // Writing to a Vec cannot fail.
        let _ = std::io::Write::write_fmt(&mut m, format_args!("{b:02x}"));
    }
    m
}

/// What a relay asks for a file token for, digested for [`relay_files_message`]: the client's
/// SSH key in its wire form, the workload, the client's address, and the token it renews, if it
/// renews one, each length-prefixed so no two requests digest alike.
#[must_use]
pub fn files_request_digest(
    key: &[u8],
    workload: uuid::Uuid,
    client: std::net::IpAddr,
    renews: Option<&str>,
) -> [u8; 32] {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    let client = client.to_string();
    for field in [key, workload.as_bytes(), client.as_bytes(), renews.unwrap_or("").as_bytes()] {
        h.update(u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        h.update(field);
    }
    h.finalize().into()
}

/// What a node signs to answer a reachability probe's `nonce` on `port` (§9 of
/// docs/07-networking.md): the same form as [`hello_message`] under
/// `korpis.agent.v1.Reachability`, with the port in place of the timestamp, so the answer proves
/// that the machine reached is the node, and is good for nothing else.
#[must_use]
pub fn probe_answer_message(node: &str, port: u16, nonce: &[u8]) -> Vec<u8> {
    let mut s = format!("korpis.agent.v1.Reachability|{node}|{port}|");
    for b in nonce {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
    }
    s.into_bytes()
}

fn proof_message(purpose: &str, node: &str, seconds: i64, nanos: u32, nonce: &[u8]) -> Vec<u8> {
    let mut s = format!("{purpose}|{node}|{seconds}.{nanos:09}|");
    for b in nonce {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
    }
    s.into_bytes()
}

/// Whether `public` is a usable Ed25519 public key: 32 bytes naming a point on the curve.
#[must_use]
pub fn valid_public_key(public: &[u8]) -> bool {
    <[u8; PUBLIC_KEY_LEN]>::try_from(public)
        .is_ok_and(|bytes| VerifyingKey::from_bytes(&bytes).is_ok())
}

/// Whether `signature` is `public`'s signature over `message`. Strict verification: a signature
/// that verifies only under the lax rules some libraries accept is refused.
#[must_use]
pub fn verify(public: &[u8], message: &[u8], signature: &[u8]) -> bool {
    let Ok(bytes) = <[u8; PUBLIC_KEY_LEN]>::try_from(public) else { return false };
    let Ok(key) = VerifyingKey::from_bytes(&bytes) else { return false };
    let Ok(sig) = Signature::from_slice(signature) else { return false };
    key.verify_strict(message, &sig).is_ok()
}

/// The node's signature over `message`: what the agent sends.
#[must_use]
pub fn sign(key: &SigningKey, message: &[u8]) -> Vec<u8> {
    key.sign(message).to_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hello_verifies_only_under_its_own_key_and_exact_string() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let public = key.verifying_key().to_bytes();
        let message = hello_message("nd_1", 1_760_000_000, 5, &[0xab, 0x01]);
        assert_eq!(message, b"korpis.agent.v1.Hello|nd_1|1760000000.000000005|ab01".to_vec());
        let signature = sign(&key, &message);
        assert!(verify(&public, &message, &signature));
        let later = hello_message("nd_1", 1_760_000_001, 5, &[0xab, 0x01]);
        assert!(!verify(&public, &later, &signature), "the timestamp is signed");
        let other = SigningKey::from_bytes(&[4; 32]).verifying_key().to_bytes();
        assert!(!verify(&other, &message, &signature));
        assert!(valid_public_key(&public));
        assert!(!valid_public_key(&public[..31]));
    }

    #[test]
    fn a_data_hello_names_the_relay_dialled_after_everything_else() {
        let m = data_hello_message(("nd_1", "https://r1.example"), 1_760_000_000, 5, &[0xab]);
        let expected = b"korpis.agent.v1.DataHello|nd_1|1760000000.000000005|ab|https://r1.example";
        assert_eq!(m, expected.to_vec());
        let other = data_hello_message(("nd_1", "https://r2.example"), 1_760_000_000, 5, &[0xab]);
        assert_ne!(m, other, "a hello for one relay is not one for another");
    }
}
