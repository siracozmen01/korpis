// SPDX-License-Identifier: AGPL-3.0-or-later

//! An operator's extension policy: set on a root organization, it governs every install beneath
//! it, a dry run included, by the registry an extension comes from, the publishers one of whom
//! must have signed it, and whether anyone without `extension.install` over the root may install
//! one; only a root's policy governs, and a change is made against the version read.

use crate::domains::child;
use crate::extensions::{install, push_dns};
use crate::recipes::project;
use crate::support::{Harness, mutation, reason, signer_of, test_signer};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

async fn set(h: &Harness, key: &str, policy: pb::ExtensionPolicy, version: u64) -> u64 {
    let req = pb::SetExtensionPolicyRequest {
        mutation: pb::Mutation { expected_version: Some(version), ..mutation(key) }.into(),
        organization: h.root_org(),
        policy: policy.into(),
        ..Default::default()
    };
    h.extensions(h.root()).set_extension_policy(req).await.unwrap().into_owned().version
}

/// What a dry run of installing the DNS extension in `project` of the root organization answers.
async fn dry(h: &Harness, repo: &str, project: &str) -> Result<(), connectrpc::ConnectError> {
    let req = pb::InstallExtensionRequest { dry_run: true, ..install(h, "dry", repo, project) };
    h.extensions(h.root()).install_extension(req).await.map(|_| ())
}

#[tokio::test]
async fn an_operator_admits_extensions_by_registry_and_publisher() {
    let h = Harness::new().await;
    let repo = push_dns().await;
    let project = project(&h, &h.root_org(), "dns").await;
    assert!(dry(&h, &repo, &project).await.is_ok(), "open by default");

    let elsewhere = pb::ExtensionPolicy {
        registries: vec!["registry.example/korpis".into()],
        tenants_install: true,
        ..Default::default()
    };
    let v = set(&h, "a", elsewhere, 0).await;
    let e = dry(&h, &repo, &project).await.unwrap_err();
    assert_eq!(reason(&e), "extension.not_allowed", "not from a registry it lists");

    let registry = repo.rsplit_once('/').unwrap().0.to_owned();
    let stranger = p256::ecdsa::SigningKey::from_slice(&[9; 32]).unwrap();
    let unsigned_by = pb::ExtensionPolicy {
        registries: vec![registry.clone()],
        publishers: vec![signer_of("stranger", &stranger)],
        tenants_install: true,
        ..Default::default()
    };
    let v = set(&h, "b", unsigned_by, v).await;
    let e = dry(&h, &repo, &project).await.unwrap_err();
    assert_eq!(reason(&e), "extension.not_allowed", "signed by no publisher it lists");

    let ours = pb::ExtensionPolicy {
        registries: vec![registry],
        publishers: vec![test_signer()],
        tenants_install: true,
        ..Default::default()
    };
    set(&h, "c", ours, v).await;
    assert!(dry(&h, &repo, &project).await.is_ok());
    assert_eq!(h.effects("extension.policy.set").await.len(), 3);
}

#[tokio::test]
async fn only_a_root_keeps_a_policy_and_a_change_is_made_against_what_was_read() {
    let h = Harness::new().await;
    let reseller = child(&h, "reseller").await;
    let req = pb::SetExtensionPolicyRequest {
        mutation: mutation("child").into(),
        organization: reseller,
        policy: pb::ExtensionPolicy::default().into(),
        ..Default::default()
    };
    let e = h.extensions(h.root()).set_extension_policy(req).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument, "a child cannot loosen the operator's rule");

    set(&h, "first", pb::ExtensionPolicy { tenants_install: true, ..Default::default() }, 0).await;
    let stale = pb::SetExtensionPolicyRequest {
        mutation: pb::Mutation { expected_version: Some(0), ..mutation("stale") }.into(),
        organization: h.root_org(),
        policy: pb::ExtensionPolicy::default().into(),
        ..Default::default()
    };
    let e = h.extensions(h.root()).set_extension_policy(stale).await.unwrap_err();
    assert_eq!(reason(&e), "extension_policy.moved");
}
