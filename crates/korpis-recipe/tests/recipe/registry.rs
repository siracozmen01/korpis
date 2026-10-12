// SPDX-License-Identifier: AGPL-3.0-or-later

//! The registry client against a real registry.

use crate::support::{PAPER, artifact, registry, repository};
use korpis_recipe::artifact::check_files;
use korpis_recipe::registry::RegistryError;

#[tokio::test]
async fn a_pushed_recipe_resolves_to_its_digest_and_pulls_back_verified() {
    let (reg, repo) = (registry(), repository());
    let pushed = reg.push(&repo, "1.21.4", &artifact(PAPER)).await.unwrap();
    assert!(korpis_core::recipe::reference::is_digest(&pushed), "{pushed}");
    assert_eq!(reg.resolve(&repo, "1.21.4").await.unwrap(), pushed);
    let pulled = reg.pull(&repo, &pushed).await.unwrap();
    assert_eq!(pulled.document, PAPER.as_bytes());
    let recipe = korpis_recipe::parse(&pulled.document).unwrap();
    check_files(&recipe, &pulled.files).unwrap();
}

#[tokio::test]
async fn a_missing_version_is_not_found_and_a_wrong_digest_pulls_nothing() {
    let (reg, repo) = (registry(), repository());
    assert!(matches!(reg.resolve(&repo, "nope").await, Err(RegistryError::NotFound(_))));
    let wrong = format!("sha256:{}", "0".repeat(64));
    assert!(reg.pull(&repo, &wrong).await.is_err(), "a digest that names nothing pulls nothing");
}

#[tokio::test]
async fn what_could_not_be_pulled_is_not_pushed() {
    let (reg, repo) = (registry(), repository());
    let mut thin = artifact(PAPER);
    thin.files.pop();
    let e = reg.push(&repo, "1", &thin).await.unwrap_err();
    assert!(matches!(&e, RegistryError::Invalid(m) if m.contains("templates/server.properties")));
    let e = reg.push(&repo, "1", &artifact("kind: Recipe")).await.unwrap_err();
    assert!(matches!(e, RegistryError::Invalid(_)), "{e}");
    assert!(matches!(reg.resolve(&repo, "1").await, Err(RegistryError::NotFound(_))));
}

#[tokio::test]
async fn signatures_are_read_beside_the_recipe_and_each_signer_is_kept() {
    use korpis_core::recipe::trust::cosign::{public_pem, signer, verified};
    let (reg, repo) = (registry(), repository());
    let digest = reg.push(&repo, "1.21.4", &artifact(PAPER)).await.unwrap();
    assert!(reg.signatures(&repo, &digest).await.unwrap().is_empty(), "never signed");
    let (ours, theirs) = (key(7), key(8));
    reg.sign(&repo, &digest, &ours).await.unwrap();
    reg.sign(&repo, &digest, &theirs).await.unwrap();
    let found = reg.signatures(&repo, &digest).await.unwrap();
    assert_eq!(found.len(), 2, "a second signature keeps the first");
    let us = signer("us", &public_pem(&ours).unwrap()).unwrap();
    let them = signer("them", &public_pem(&theirs).unwrap()).unwrap();
    assert_eq!(verified(&digest, &found, &[us.clone(), them.clone()]).len(), 2);
    let other = reg.push(&repo, "1.21.5", &artifact(&PAPER.replace("1.21.4", "1.21.5"))).await;
    let other = other.unwrap();
    assert!(verified(&other, &found, &[us, them]).is_empty(), "signatures name their digest");
}

fn key(n: u8) -> p256::ecdsa::SigningKey {
    p256::ecdsa::SigningKey::from_slice(&[n; 32]).unwrap()
}
