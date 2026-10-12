// SPDX-License-Identifier: AGPL-3.0-or-later

//! The resolve use case: a version becomes a digest once, and the store keeps the recipe.

use crate::support::{PAPER, artifact, registry, repository};
use korpis_app::App;
use korpis_app::ports::SystemClock;
use korpis_core::authz::token::SigningKey;
use korpis_recipe::registry::Registry;
use korpis_recipe::source::RegistrySource;
use korpis_store::testing::TestDb;
use std::sync::Arc;

#[tokio::test]
async fn a_resolved_recipe_outlives_its_registry() {
    let repo = repository();
    let digest = registry().push(&repo, "1.21.4", &artifact(PAPER)).await.unwrap();
    let db = TestDb::new().await.unwrap();
    let key = Arc::new(SigningKey::generate(1));

    let app = App::new(
        db.store().await.unwrap(),
        SystemClock,
        key.clone(),
        RegistrySource::new(registry()),
    );
    let (reference, recipe) = app.resolve_recipe(&repo, "1.21.4").await.unwrap();
    assert_eq!(reference.digest, digest);
    assert_eq!(recipe.metadata.name, "paper");

    // A registry that is gone: what was resolved is still there, by digest.
    let gone = RegistrySource::new(Registry::new(vec!["127.0.0.1:1".to_owned()]));
    let offline = App::new(db.store().await.unwrap(), SystemClock, key, gone);
    let (again, _) = offline.resolve_recipe(&repo, &digest).await.unwrap();
    assert_eq!(again.digest, digest);
    let e = offline.resolve_recipe(&repo, "1.21.4").await.unwrap_err();
    assert!(matches!(e, korpis_app::AppError::Unavailable { .. }), "{e:?}");
}
