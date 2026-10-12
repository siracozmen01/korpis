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
