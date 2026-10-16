// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn keys() -> TlsKeys {
    TlsKeys { key: Zeroizing::new([7; 32]) }
}

/// A self-signed certificate for `names`, in PEM, and its key.
fn issued(names: &[&str]) -> (String, String) {
    let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    let made = rcgen::generate_simple_self_signed(names).unwrap();
    (made.cert.pem(), made.signing_key.serialize_pem())
}

#[test]
fn a_chain_and_its_own_key_are_read_for_their_names_and_validity() {
    let (chain, key) = issued(&["Shop.example.com", "*.example.com"]);
    let (leaf, served) = keys().read(&chain, &key).unwrap();
    assert_eq!(leaf.names, ["shop.example.com", "*.example.com"], "lowercased");
    assert!(leaf.not_before < SystemTime::now() && SystemTime::now() < leaf.not_after);
    assert_eq!(chain_of(&served).unwrap(), chain_of(&chain).unwrap(), "the same certificates");
}

#[test]
fn another_key_or_what_is_not_a_chain_is_refused() {
    let (chain, _) = issued(&["shop.example.com"]);
    let (_, other) = issued(&["shop.example.com"]);
    assert!(matches!(keys().read(&chain, &other), Err(ReadError::Key(_))));
    assert!(matches!(keys().read("not pem", &other), Err(ReadError::Chain(_))));
    assert!(matches!(keys().read(&chain, "not a key"), Err(ReadError::Key(_))));
    let huge = "x".repeat(MAX_PEM + 1);
    assert!(matches!(keys().read(&huge, &other), Err(ReadError::Chain(_))));
}

#[test]
fn a_sealed_key_opens_for_its_certificate_alone() {
    let (one, two) = (Uuid::now_v7(), Uuid::now_v7());
    let sealed = keys().seal(one, "secret key").unwrap();
    assert_eq!(sealed.sealed_by, TLS_KEY);
    assert_eq!(keys().open(one, &sealed).unwrap().as_str(), "secret key");
    assert!(matches!(keys().open(two, &sealed), Err(VaultError::Forged(_))));
    let other = TlsKeys { key: Zeroizing::new([8; 32]) };
    assert!(matches!(other.open(one, &sealed), Err(VaultError::Forged(_))));
    let named = SealedKey { sealed_by: "backup-1".into(), ..sealed };
    assert!(matches!(keys().open(one, &named), Err(VaultError::UnknownKey(_))));
}
