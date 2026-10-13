// SPDX-License-Identifier: AGPL-3.0-or-later

//! An extension's document and artifact (§3.1 of docs/16-extensions.md): a recipe with one more
//! block, read by the same parser, and published under types of its own.

use crate::support::{PAPER, address, registry};
use korpis_app::ports::RecipeSource as _;
use korpis_core::extension::{NeedScope, ProviderType};
use korpis_core::labels::LabelExpression;
use korpis_recipe::artifact::{Artifact, EXTENSION_ARTIFACT_TYPE, Kind};
use korpis_recipe::document::{DocumentError, parse_document};
use korpis_recipe::source::RegistrySource;

const DNS: &str = include_str!("dns.yaml");

fn field(e: DocumentError) -> String {
    match e {
        DocumentError::Invalid(e) => e.field,
        DocumentError::Syntax(s) => panic!("not a rule: {s}"),
    }
}

#[test]
fn an_extension_document_reads_as_its_workload_and_its_manifest() {
    let parsed = parse_document(DNS.as_bytes()).unwrap();
    assert_eq!(parsed.kind(), Kind::Extension);
    assert_eq!(parsed.recipe.metadata.name, "cloudflare-dns");
    let m = parsed.extension.unwrap();
    assert_eq!(m.api_version, "korpis.extension.v1");
    assert_eq!(m.needs.len(), 2);
    assert!(m.needs.iter().all(|n| n.scope == NeedScope::Organization));
    assert_eq!(m.egress.len(), 2);
    assert_eq!(m.egress[1].host, "2606:4700::1111", "an IPv6 host is written in brackets");
    let filter = &m.subscribes[0];
    assert!(filter.matches_kind("endpoint.allocate") && !filter.matches_kind("workload.stop"));
    let expression = &filter.workloads.expressions[0];
    assert!(matches!(expression, LabelExpression::NotIn(k, _) if k == "tier"));
    assert_eq!(m.provides[0].kind, ProviderType::Dns);
    assert!(m.actions[0].arguments[0].kind.is_resource());
    let workload = korpis_recipe::parse(DNS.as_bytes()).unwrap();
    let same = format!("{workload:?}") == format!("{:?}", parsed.recipe);
    assert!(same, "a driver reads an extension's workload as any recipe");
    assert_eq!(parse_document(PAPER.as_bytes()).unwrap().kind(), Kind::Recipe);
}

#[test]
fn the_kind_and_the_block_go_together_and_the_block_is_checked() {
    let bare = DNS.split("\nextension:").next().unwrap();
    assert_eq!(field(parse_document(bare.as_bytes()).unwrap_err()), "extension");
    let smuggled = DNS.replace("kind: Extension", "kind: Recipe");
    assert_eq!(field(parse_document(smuggled.as_bytes()).unwrap_err()), "extension");
    let other = DNS.replace("kind: Extension", "kind: Plugin");
    assert_eq!(field(parse_document(other.as_bytes()).unwrap_err()), "apiVersion");
    let deaf = DNS.replace("actions: [event.read]", "actions: [endpoint.write]");
    assert_eq!(field(parse_document(deaf.as_bytes()).unwrap_err()), "extension.subscribes");
    let scope = DNS.replacen("scope: organization", "scope: everywhere", 1);
    assert_eq!(field(parse_document(scope.as_bytes()).unwrap_err()), "extension.needs[0].scope");
    let unknown = DNS.replace("[endpoint.read]", "[endpoint.own]");
    let e = parse_document(unknown.as_bytes()).unwrap_err();
    assert_eq!(field(e), "extension.needs[0].actions");
    let port = DNS.replace("api.cloudflare.com:443", "api.cloudflare.com");
    assert_eq!(field(parse_document(port.as_bytes()).unwrap_err()), "extension.egress[0]");
    let v6 = DNS.replace("[2606:4700::1111]:443", "2606:4700::1111:443");
    assert_eq!(field(parse_document(v6.as_bytes()).unwrap_err()), "extension.egress[1]");
    let typo = DNS.replace("ui_slots:", "ui_slot:");
    assert!(matches!(parse_document(typo.as_bytes()), Err(DocumentError::Syntax(_))));
    let operator = DNS.replace("operator: NotIn", "operator: Exists");
    let e = parse_document(operator.as_bytes()).unwrap_err();
    assert_eq!(field(e), "extension.subscribes[0].workloads.expressions[0]");
}

#[tokio::test]
async fn an_extension_is_published_as_an_extension_and_fetched_back() {
    let reg = registry();
    let repo = format!("{}/test/dns-{}", address(), uuid::Uuid::now_v7().simple());
    let artifact = Artifact { document: DNS.as_bytes().to_vec(), files: Vec::new() };
    let digest = reg.push(&repo, "1.0.0", &artifact).await.unwrap();
    let pulled = reg.pull(&repo, &digest).await.unwrap();
    assert_eq!(pulled.kind, Kind::Extension);
    assert_eq!(Kind::Extension.artifact_type(), EXTENSION_ARTIFACT_TYPE);
    let fetched = RegistrySource::new(registry()).fetch(&repo, &digest).await.unwrap();
    assert_eq!(fetched.described.recipe.metadata.name, "cloudflare-dns");
    assert!(fetched.described.extension.is_some(), "the source reports what it fetched");
}
