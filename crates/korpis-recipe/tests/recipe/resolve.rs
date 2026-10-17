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
    let gone = RegistrySource::new(Registry::new(vec!["127.0.0.1:1".to_owned()]).unwrap());
    let offline = App::new(db.store().await.unwrap(), SystemClock, key, gone);
    let (again, _) = offline.resolve_recipe(&repo, &digest).await.unwrap();
    assert_eq!(again.digest, digest);
    let e = offline.resolve_recipe(&repo, "1.21.4").await.unwrap_err();
    assert!(matches!(e, korpis_app::AppError::Unavailable { .. }), "{e:?}");
}

#[tokio::test]
async fn a_registry_on_the_control_plane_s_own_network_is_reached_only_when_allowed() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Something listening on loopback, standing in for a service the control plane can reach.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&connections);
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            drop(s);
        }
    });
    let repo = format!("{at}/internal/thing");
    let e = Registry::new(Vec::new()).unwrap().resolve(&repo, "1").await.unwrap_err();
    assert!(e.to_string().contains("tunnel"), "the proxy refused it, and says so: {e}");
    assert_eq!(connections.load(Ordering::SeqCst), 0, "nothing reached it");
    let reach = korpis_egress::Reach::with(vec!["127.0.0.1/32".parse().unwrap()]);
    let allowed = Registry::reaching(Vec::new(), reach).unwrap();
    assert!(allowed.resolve(&repo, "1").await.is_err(), "it is no registry");
    assert!(connections.load(Ordering::SeqCst) > 0, "but it was reached");
}

#[tokio::test]
async fn a_recipe_s_catalogues_are_read_with_it_and_kept_with_it() {
    let repo = repository();
    let mut published = artifact(PAPER);
    published.files.push(korpis_recipe::artifact::File {
        path: "i18n/tr.json".to_owned(),
        media_type: "application/json".to_owned(),
        data: br#"{"config.fields.max_players.label": "En fazla oyuncu"}"#.to_vec(),
    });
    let digest = registry().push(&repo, "1.21.4", &published).await.unwrap();
    let db = TestDb::new().await.unwrap();
    let key = Arc::new(SigningKey::generate(1));
    let app = App::new(
        db.store().await.unwrap(),
        SystemClock,
        key.clone(),
        RegistrySource::new(registry()),
    );
    let (_, described) = app.described_recipe(&repo, "1.21.4").await.unwrap();
    assert_eq!(described.catalogues["tr"]["config.fields.max_players.label"], "En fazla oyuncu");

    // Read back from the store, with the registry gone, it still speaks Turkish.
    let gone = RegistrySource::new(Registry::new(vec!["127.0.0.1:1".to_owned()]).unwrap());
    let offline = App::new(db.store().await.unwrap(), SystemClock, key, gone);
    let (_, kept) = offline.described_recipe(&repo, &digest).await.unwrap();
    assert_eq!(kept.catalogues["tr"].len(), 1);

    // A catalogue naming a message the recipe does not show is not pushed, nor would it load.
    let mut wrong = artifact(PAPER);
    wrong.files.push(korpis_recipe::artifact::File {
        path: "i18n/tr.json".to_owned(),
        media_type: "application/json".to_owned(),
        data: br#"{"config.fields.nothing.label": "x"}"#.to_vec(),
    });
    let e = registry().push(&repo, "bad", &wrong).await.unwrap_err();
    assert!(e.to_string().contains("not a message the recipe shows"), "{e}");
}
