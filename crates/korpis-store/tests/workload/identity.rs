// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity bindings and their provider's calls in the store (§5 and §5.1 of
//! docs/08-identity.md): a due binding is claimed by one control plane at a time, an answer is
//! recorded only against the count it was asked at, and a finished call keeps none of the
//! evidence a person presented.

use crate::support::{Fixture, alice, fixture, now};
use korpis_app::ports::{BindingRead, BindingWrite, Store, Tx};
use korpis_core::authz::binding::{
    BindingOrigin, BindingState, BindingTerms, IdentityBinding, ProviderRef,
};
use korpis_core::principal::Principal;
use std::time::Duration;
use uuid::Uuid;

async fn bound(f: &Fixture) -> IdentityBinding {
    let provider = ProviderRef { organization: f.organization, name: "corp".into() };
    let b = IdentityBinding {
        id: Uuid::now_v7(),
        subject: Principal::ExternalSet {
            provider: provider.key(),
            realm: "groups".into(),
            member_of: "admins".into(),
        },
        provider,
        origin: BindingOrigin::Operator,
        terms: BindingTerms::default(),
        state: BindingState::Unverified,
        last_verified_at: None,
        verifications: 0,
        version: 1,
        made_by: alice(),
        made_at: now(),
        retired_at: None,
    };
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_binding(&b).await.unwrap();
    tx.commit().await.unwrap();
    b
}

#[tokio::test]
async fn a_due_binding_is_claimed_once_and_an_answer_recorded_only_against_its_count() {
    let f = fixture().await;
    let b = bound(&f).await;
    let at = now() + Duration::from_secs(1);
    let mut tx = f.store.begin().await.unwrap();
    let claimed = tx.claim_due_bindings(at, at + Duration::from_secs(30), 8).await.unwrap();
    assert_eq!(claimed.iter().map(|c| c.id).collect::<Vec<_>>(), [b.id], "due when made");
    let again = tx.claim_due_bindings(at, at + Duration::from_secs(30), 8).await.unwrap();
    assert!(again.is_empty(), "held by the claim");
    let due = at + Duration::from_secs(900);
    let verified = BindingState::Verified;
    assert!(tx.record_verification((b.id, 0), verified, Some(at), due).await.unwrap());
    let stale = tx.record_verification((b.id, 0), BindingState::Unverified, None, due);
    assert!(!stale.await.unwrap(), "an answer asked before the one recorded is not recorded");
    tx.commit().await.unwrap();
    let read = f.store.binding(b.id).await.unwrap().unwrap();
    assert_eq!((read.state, read.verifications), (verified, 1));
}

#[tokio::test]
async fn a_finished_call_keeps_none_of_the_evidence_presented() {
    let f = fixture().await;
    let id = Uuid::now_v7();
    let request = serde_json::json!({ "assert": {
        "evidence": "6b6f7270697363", "headers": { "x-signature-ed25519": "ab" } } });
    sqlx::query(
        "INSERT INTO provider_call (id, organization_id, type, name, request, deadline, created_at)
         VALUES ($1, $2, 'identity', 'corp', $3, now() + interval '1 minute', now())",
    )
    .bind(id)
    .bind(f.organization)
    .bind(&request)
    .execute(f.store.pool())
    .await
    .unwrap();
    let read = || async {
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT request FROM provider_call WHERE id = $1",
        )
        .bind(id)
        .fetch_one(f.store.pool())
        .await
        .unwrap()
    };
    assert_eq!(read().await, request, "kept while it is queued");
    sqlx::query("UPDATE provider_call SET outcome = '{}', finished_at = now() WHERE id = $1")
        .bind(id)
        .execute(f.store.pool())
        .await
        .unwrap();
    let kept = read().await;
    assert_eq!(kept["assert"]["evidence"], "", "{kept}");
    assert_eq!(kept["assert"]["headers"], serde_json::json!({}));
}

#[tokio::test]
async fn an_edit_voids_answers_asked_before_it_and_an_ending_is_recorded_whatever_came_between() {
    let f = fixture().await;
    let b = bound(&f).await;
    let at = now() + Duration::from_secs(1);
    let later = at + Duration::from_secs(3600);
    let verified = BindingState::Verified;
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.record_verification((b.id, 0), verified, Some(at), later).await.unwrap());
    let terms = BindingTerms { reverify_interval: Duration::from_secs(120), ..b.terms.clone() };
    assert!(tx.update_binding((b.id, 1), &terms, at).await.unwrap());
    let asked_before = tx.record_verification((b.id, 1), verified, Some(at), later);
    assert!(!asked_before.await.unwrap(), "asked under the old terms");
    let due = tx.claim_due_bindings(at, at + Duration::from_secs(30), 8).await.unwrap();
    assert_eq!(due.len(), 1, "due again at once under its new terms");
    assert_eq!(tx.record_ended(b.id, at).await.unwrap(), Some(verified), "whatever the count");
    tx.commit().await.unwrap();
    let read = f.store.binding(b.id).await.unwrap().unwrap();
    assert_eq!((read.state, read.verifications), (BindingState::Unverified, 3));
}
