// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipe trust policies (§7 of docs/09-recipes.md): decided on the digest at declaration,
//! recorded in the intent, inherited from the nearest organization that set one.

use crate::recipes::{body, change, create, project, push_paper_signed_by, push_unsigned_paper};
use crate::support::{
    Harness, TEST_SIGNER, detail, mutation, reason, signer_of, test_key, test_signer,
};
use korpis_proto::korpis::v1 as pb;

fn trust(declared: &pb::DeclareIntentResponse) -> pb::RecipeTrust {
    let intent = declared.intent.as_option().unwrap();
    let body = intent.body.as_option().unwrap();
    body.recipe.as_option().unwrap().trust.as_option().unwrap().clone()
}

fn set(
    key: &str,
    org: &str,
    policy: Option<pb::TrustPolicy>,
    expected: u64,
) -> pb::SetTrustPolicyRequest {
    pb::SetTrustPolicyRequest {
        mutation: pb::Mutation {
            idempotency_key: key.to_owned(),
            expected_version: (expected > 0).then_some(expected),
            ..Default::default()
        }
        .into(),
        organization: org.to_owned(),
        policy: policy.into(),
        ..Default::default()
    }
}

fn accepting(signers: Vec<pb::Signer>, allow_unsigned: bool) -> pb::TrustPolicy {
    pb::TrustPolicy { accepted_signers: signers, allow_unsigned, ..Default::default() }
}

async fn governing(h: &Harness, org: &str) -> pb::GoverningTrustPolicy {
    let request = pb::GetTrustPolicyRequest { organization: org.to_owned(), ..Default::default() };
    let got = h.orgs(h.root()).get_trust_policy(request).await.unwrap().into_owned();
    got.governing.as_option().unwrap().clone()
}

async fn child(h: &Harness, name: &str) -> String {
    let created = h
        .orgs(h.root())
        .create_organization(pb::CreateOrganizationRequest {
            mutation: mutation(&format!("org-{name}")).into(),
            name: name.into(),
            parent: Some(h.root_org()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    created.organization.as_option().unwrap().id.clone()
}

#[tokio::test]
async fn a_recipe_no_accepted_signer_signed_is_refused_unless_unsigned_use_is_allowed() {
    let h = Harness::new().await;
    let (repo, digest) = push_unsigned_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    let refused = workloads.declare_intent(create("d1", &project, "a", body(&repo, &[]))).await;
    let e = refused.unwrap_err();
    assert_eq!((reason(&e), detail(&e).field), ("recipe.untrusted".into(), "body.recipe".into()));
    assert!(e.message.as_deref().unwrap_or_default().contains(&digest), "names the digest");

    let policy = accepting(vec![test_signer()], true);
    h.orgs(h.root()).set_trust_policy(set("p1", &h.root_org(), Some(policy), 1)).await.unwrap();
    let declared = workloads
        .declare_intent(create("d2", &project, "a", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    assert!(trust(&declared).unsigned, "the intent records that nothing vouched for it");
    let workload = declared.workload.as_option().unwrap().id.clone();
    let listed = h
        .effect_log(h.root())
        .list_effects(pb::ListEffectsRequest { workload: Some(workload), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    let unsigned: Vec<_> =
        listed.effects.iter().filter(|e| e.action == "recipe.use_unsigned").collect();
    assert_eq!(unsigned.len(), 1, "every unsigned use is logged");
}

#[tokio::test]
async fn the_intent_records_the_signer_that_vouched() {
    let h = Harness::new().await;
    let (repo, _) = push_paper_signed_by(&test_key()).await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("d", &project, "a", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    assert_eq!(trust(&declared).signer, TEST_SIGNER);
    let g = governing(&h, &h.root_org()).await;
    assert_eq!((g.set_on.as_deref(), g.own_version), (Some(h.root_org().as_str()), 1));
    let signers = &g.policy.as_option().unwrap().accepted_signers;
    assert!(signers.iter().all(|s| s.fingerprint.starts_with("sha256:")), "computed, not sent");
}

#[tokio::test]
async fn a_child_inherits_until_it_sets_its_own_and_again_once_it_removes_it() {
    let h = Harness::new().await;
    let customer = child(&h, "customer").await;
    let g = governing(&h, &customer).await;
    assert_eq!((g.set_on, g.own_version), (Some(h.root_org()), 0), "the root's governs");

    let theirs = p256::ecdsa::SigningKey::from_slice(&[9; 32]).unwrap();
    let (ours_repo, _) = push_paper_signed_by(&test_key()).await;
    let (their_repo, _) = push_paper_signed_by(&theirs).await;
    let project = project(&h, &customer, "games").await;
    let orgs = h.orgs(h.root());
    let own = accepting(vec![signer_of("customer-ci", &theirs)], false);
    orgs.set_trust_policy(set("c1", &customer, Some(own), 0)).await.unwrap();

    let workloads = h.workloads(h.root());
    let e = workloads
        .declare_intent(create("d1", &project, "a", body(&ours_repo, &[])))
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "recipe.untrusted", "its own policy replaces the root's");
    let declared = workloads
        .declare_intent(create("d2", &project, "a", body(&their_repo, &[])))
        .await
        .unwrap()
        .into_owned();
    assert_eq!(trust(&declared).signer, "customer-ci");

    let removed = orgs.set_trust_policy(set("c2", &customer, None, 1)).await.unwrap().into_owned();
    let g = removed.governing.as_option().unwrap();
    assert_eq!((g.set_on.clone(), g.own_version), (Some(h.root_org()), 2), "inherits again");
    let declared = workloads
        .declare_intent(create("d3", &project, "b", body(&ours_repo, &[])))
        .await
        .unwrap()
        .into_owned();
    assert_eq!(trust(&declared).signer, TEST_SIGNER);
}

#[tokio::test]
async fn removing_a_signer_refuses_the_next_declaration_but_not_what_was_declared() {
    let h = Harness::new().await;
    let (repo, _) = push_paper_signed_by(&test_key()).await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    let declared = workloads
        .declare_intent(create("d1", &project, "a", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let workload = declared.workload.as_option().unwrap();
    let orgs = h.orgs(h.root());
    let none = accepting(Vec::new(), false);
    orgs.set_trust_policy(set("p1", &h.root_org(), Some(none), 1)).await.unwrap();
    let e = workloads
        .declare_intent(change("d2", &workload.id, 1, body(&repo, &[])))
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "recipe.untrusted");
    let read = workloads
        .get_workload(pb::GetWorkloadRequest { id: workload.id.clone(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    let status = read.status.as_option().unwrap();
    let head = status.head.as_option().unwrap();
    assert_eq!(head.id, declared.intent.as_option().unwrap().id, "the declared intent stands");
}

#[tokio::test]
async fn a_policy_is_validated_and_changed_by_compare_and_swap() {
    let h = Harness::new().await;
    let orgs = h.orgs(h.root());
    let root = h.root_org();
    let twice = accepting(vec![test_signer(), test_signer()], false);
    let e = orgs.set_trust_policy(set("v1", &root, Some(twice), 1)).await.unwrap_err();
    assert_eq!(detail(&e).field, "policy.accepted_signers[1].name");
    let mut renamed = test_signer();
    renamed.name = "again".into();
    let same_key = accepting(vec![test_signer(), renamed], false);
    let e = orgs.set_trust_policy(set("v2", &root, Some(same_key), 1)).await.unwrap_err();
    assert_eq!(detail(&e).field, "policy.accepted_signers[1].public_key_pem");
    let mut garbled = test_signer();
    garbled.public_key_pem = "not a key".into();
    let e = orgs.set_trust_policy(set("v3", &root, Some(accepting(vec![garbled], false)), 1));
    assert_eq!(reason(&e.await.unwrap_err()), "request.invalid");

    let stale = orgs.set_trust_policy(set("v4", &root, Some(accepting(Vec::new(), true)), 0));
    assert_eq!(reason(&stale.await.unwrap_err()), "trust_policy.moved");
    let customer = child(&h, "customer").await;
    let e = orgs.set_trust_policy(set("v5", &customer, None, 0)).await.unwrap_err();
    assert_eq!(reason(&e), "trust_policy.not_set");
}
