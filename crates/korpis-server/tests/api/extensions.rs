// SPDX-License-Identifier: AGPL-3.0-or-later

//! Extensions through the API (docs/16-extensions.md): an extension's artifact is installed,
//! never declared as a workload of its own.

use crate::recipes::{body, create, project, registry_address};
use crate::support::{Harness, detail, test_key};
use connectrpc::ErrorCode;
use korpis_recipe::artifact::Artifact;
use korpis_recipe::registry::Registry;
use uuid::Uuid;

/// The DNS extension the recipe crate's tests load (§3.1 of docs/16-extensions.md).
const DNS: &str = include_str!("../../../korpis-recipe/tests/recipe/dns.yaml");

/// Pushes the DNS extension, signed by the signer every harness's root organization accepts;
/// returns the repository.
pub async fn push_dns() -> String {
    let repo = format!("{}/test/dns-{}", registry_address(), Uuid::now_v7().simple());
    let registry = Registry::new(vec![registry_address()]);
    let artifact = Artifact { document: DNS.as_bytes().to_vec(), files: Vec::new() };
    let digest = registry.push(&repo, "1.0.0", &artifact).await.unwrap();
    registry.sign(&repo, &digest, &test_key()).await.unwrap();
    repo
}

#[tokio::test]
async fn an_extension_is_installed_and_never_declared_as_a_plain_workload() {
    let h = Harness::new().await;
    let repo = push_dns().await;
    let project = project(&h, &h.root_org(), "dns").await;
    let mut declared = body(&repo, &[]);
    if let Some(r) = declared.recipe.as_option_mut() {
        r.version = "1.0.0".into();
    }
    let e = h
        .workloads(h.root())
        .declare_intent(create("plain", &project, "dns", declared))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    let d = detail(&e);
    assert_eq!(d.field, "recipe");
    assert!(d.message.contains("installed, not declared"), "{}", d.message);
}
