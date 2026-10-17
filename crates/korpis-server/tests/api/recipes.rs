// SPDX-License-Identifier: AGPL-3.0-or-later

//! A recipe in the development registry, and declarations of it.

use crate::support::{Harness, mutation, test_key};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use korpis_proto::korpis::v1::__buffa::oneof::value::Kind as V;
use korpis_recipe::artifact::{Artifact, File};
use korpis_recipe::registry::Registry;
use uuid::Uuid;

/// The example recipe of §3 of docs/09-recipes.md, as the recipe crate's tests load it.
pub const PAPER: &str = include_str!("../../../korpis-recipe/tests/recipe/paper.yaml");

pub fn registry_address() -> String {
    std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned())
}

/// Pushes the example, unsigned, to a repository no other test uses; returns the repository
/// and the digest it pushed.
pub async fn push_unsigned_paper() -> (String, String) {
    push_unsigned_document(PAPER).await
}

/// Pushes `document` with the example's files, signed as [`push_paper`] is; returns the
/// repository.
pub async fn push_paper_like(document: &str) -> String {
    push_paper_with(document, Vec::new()).await
}

/// Pushes `document` with the example's files and `more`, signed as [`push_paper`] is; returns
/// the repository.
pub async fn push_paper_with(document: &str, more: Vec<File>) -> String {
    let (repo, digest) = push_document(document, more).await;
    Registry::new(vec![registry_address()])
        .unwrap()
        .sign(&repo, &digest, &test_key())
        .await
        .unwrap();
    repo
}

/// Pushes `document` with the example's files, unsigned, as [`push_unsigned_paper`] does.
async fn push_unsigned_document(document: &str) -> (String, String) {
    push_document(document, Vec::new()).await
}

/// Pushes `document` with the example's files and `more`, unsigned.
async fn push_document(document: &str, more: Vec<File>) -> (String, String) {
    let repo = format!("{}/test/paper-{}", registry_address(), Uuid::now_v7().simple());
    let file = |path: &str, data: &[u8]| File {
        path: path.to_owned(),
        media_type: "application/octet-stream".to_owned(),
        data: data.to_vec(),
    };
    let artifact = Artifact {
        document: document.as_bytes().to_vec(),
        files: [
            vec![
                file("layers/rootfs.tar.zst", b"not really a rootfs"),
                file("templates/server.properties", b"motd={{motd}}\n"),
            ],
            more,
        ]
        .concat(),
    };
    let registry = Registry::new(vec![registry_address()]).unwrap();
    let digest = registry.push(&repo, "1.21.4", &artifact).await.unwrap();
    (repo, digest)
}

/// Pushes the example signed by `key`; returns the repository and the digest.
pub async fn push_paper_signed_by(key: &p256::ecdsa::SigningKey) -> (String, String) {
    let (repo, digest) = push_unsigned_paper().await;
    Registry::new(vec![registry_address()]).unwrap().sign(&repo, &digest, key).await.unwrap();
    (repo, digest)
}

/// Pushes the example signed by the signer every harness's root organization accepts;
/// returns the repository.
pub async fn push_paper() -> String {
    push_paper_signed_by(&test_key()).await.0
}

pub fn text(s: &str) -> pb::Value {
    pb::Value { kind: Some(V::StringValue(s.to_owned())), ..Default::default() }
}

/// A declaration of the example that states only its recipe and `config`.
pub fn body(repo: &str, config: &[(&str, pb::Value)]) -> pb::IntentBody {
    pb::IntentBody {
        recipe: pb::RecipeRef {
            name: repo.to_owned(),
            version: "1.21.4".into(),
            ..Default::default()
        }
        .into(),
        config: config.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect(),
        ..Default::default()
    }
}

/// A declaration creating workload `name` in `project`.
pub fn create(
    key: &str,
    project: &str,
    name: &str,
    body: pb::IntentBody,
) -> pb::DeclareIntentRequest {
    pb::DeclareIntentRequest {
        mutation: mutation(key).into(),
        target: Some(Target::Create(Box::new(pb::NewWorkload {
            project: project.to_owned(),
            name: name.to_owned(),
            ..Default::default()
        }))),
        body: body.into(),
        ..Default::default()
    }
}

/// A declaration of a new intent for `workload`, made against head `version`.
pub fn change(
    key: &str,
    workload: &str,
    version: u64,
    body: pb::IntentBody,
) -> pb::DeclareIntentRequest {
    pb::DeclareIntentRequest {
        mutation: pb::Mutation {
            idempotency_key: key.to_owned(),
            expected_version: Some(version),
            ..Default::default()
        }
        .into(),
        target: Some(Target::Workload(workload.to_owned())),
        body: body.into(),
        ..Default::default()
    }
}

/// A project in the root organization.
pub async fn project(h: &Harness, org: &str, name: &str) -> String {
    let created = h
        .projects(h.root())
        .create_project(pb::CreateProjectRequest {
            mutation: mutation(&format!("project-{name}")).into(),
            organization: org.to_owned(),
            name: name.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    created.project.as_option().unwrap().id.clone()
}
