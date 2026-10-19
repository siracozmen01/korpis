// SPDX-License-Identifier: AGPL-3.0-or-later

//! The example recipe of §3 of docs/09-recipes.md, pushed to a repository of each test's own.

use korpis_recipe::artifact::{Artifact, File};
use korpis_recipe::registry::Registry;

/// The example recipe.
const PAPER: &str = include_str!("../../../../korpis-recipe/tests/recipe/paper.yaml");

/// Pushes the example to a repository no other test uses; returns the repository.
pub async fn push_paper() -> String {
    push_document(PAPER).await
}

/// Pushes the example recipe with its pre-stop hook waiting `wait` (e.g. `2s`) for the server
/// to exit rather than its own two minutes; the repository.
pub async fn push_paper_waiting(wait: &str) -> String {
    let hook = "wait_for_exit: 120s";
    assert!(PAPER.contains(hook), "the example recipe's hook");
    push_document(&PAPER.replace(hook, &format!("wait_for_exit: {wait}"))).await
}

async fn push_document(document: &str) -> String {
    let registry =
        std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned());
    let repo = format!("{registry}/test/paper-{}", uuid::Uuid::now_v7().simple());
    let file = |path: &str, data: &[u8]| File {
        path: path.to_owned(),
        media_type: "application/octet-stream".to_owned(),
        data: data.to_vec(),
    };
    let artifact = Artifact {
        document: document.as_bytes().to_vec(),
        files: vec![
            file("layers/rootfs.tar.zst", b"not really a rootfs"),
            file("templates/server.properties", b"motd={{motd}}\n"),
        ],
    };
    Registry::new(vec![registry]).unwrap().push(&repo, "1.21.4", &artifact).await.unwrap();
    repo
}
