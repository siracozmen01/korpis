// SPDX-License-Identifier: AGPL-3.0-or-later

//! The ACME adapter against Pebble (§3.2 of docs/07-networking.md): an account is made only with
//! the terms agreed and is kept, an order's challenges, HTTP-01 and DNS-01 for each name, are
//! handed over before the directory is told to look, and what it issues is a chain whose leaf the fresh key it came with matches.

use korpis_app::ports::{Acme, AcmeError, Challenge, Proof, ReadError, TlsVault};
use korpis_server::acme::{AcmeClient, AcmeConfig};
use korpis_server::data::DataDir;
use korpis_server::tls_vault::TlsKeys;
use std::sync::{Arc, Mutex};

/// Says every name was proven over HTTP, publishing nothing.
fn by_http<'a>(c: Vec<Challenge>) -> korpis_app::ports::AcmeFuture<'a, Vec<Proof>> {
    Box::pin(async move { Ok(vec![Proof::Http01; c.len()]) })
}

#[tokio::test]
async fn an_order_hands_over_its_challenges_and_issues_for_a_fresh_key() {
    let Some(pebble) = crate::pebble::start(true).await else { return };
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path().join("data"));
    std::fs::create_dir_all(data.keys()).unwrap();

    let refused = AcmeClient::new(AcmeConfig { agreed: false, ..pebble.config() }, &data);
    let names = vec!["shop.example.com".to_owned(), "www.example.com".to_owned()];
    let none: korpis_app::ports::Publish<'_> = Box::new(by_http);
    let e = refused.issue(&names, none).await.unwrap_err();
    assert!(matches!(e, AcmeError::Terms(_)), "{e}");

    let client = AcmeClient::new(pebble.config(), &data);
    let seen: Arc<Mutex<Vec<Challenge>>> = Arc::default();
    let into = Arc::clone(&seen);
    let publish: korpis_app::ports::Publish<'_> = Box::new(move |c| {
        *into.lock().unwrap() = c.clone();
        by_http(c)
    });
    let issued = client.issue(&names, publish).await.unwrap();
    let seen = seen.lock().unwrap().clone();
    let mut asked: Vec<&str> = seen.iter().map(|c| c.name.as_str()).collect();
    asked.sort_unstable();
    assert_eq!(asked, ["shop.example.com", "www.example.com"]);
    for c in &seen {
        let http = c.http.as_ref().expect("HTTP-01 offered");
        assert!(http.key_authorization.starts_with(&format!("{}.", http.token)));
        assert!(c.dns_value.as_ref().is_some_and(|v| v.len() == 43), "DNS-01 offered");
    }

    let tls = TlsKeys::load_or_create(&data).unwrap();
    let (leaf, _) = tls.read(&issued.chain, &issued.key).unwrap();
    let mut issued_for = leaf.names.clone();
    issued_for.sort_unstable();
    assert_eq!(issued_for, ["shop.example.com", "www.example.com"]);
    let kept = std::fs::read_dir(data.keys()).unwrap().flatten();
    assert_eq!(kept.filter(|e| e.file_name().to_string_lossy().starts_with("acme-")).count(), 1);

    let again = AcmeClient::new(pebble.config(), &data);
    let none: korpis_app::ports::Publish<'_> = Box::new(by_http);
    let other = again.issue(&names[..1], none).await.unwrap();
    let fresh = tls.read(&other.chain, &issued.key);
    assert!(matches!(fresh, Err(ReadError::Key(_))), "each order makes a key of its own");
}
