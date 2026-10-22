// SPDX-License-Identifier: AGPL-3.0-or-later

//! The members of a bound set who delegated, in the store (§5.1 of docs/08-identity.md): a
//! grant naming a member in its `via` records them, unverified, and makes the set's binding due;
//! re-verification reads the members whose grants still stand and records what it found, never
//! an answer that a member is still there over a record written since it asked (Finding 141).

use crate::support::{Fixture, alice, fixture, now};
use korpis_app::ports::{BindingWrite, GrantWrite, MemberRead, MemberWrite, Store, Tx};
use korpis_core::authz::binding::{
    BindingOrigin, BindingState, BindingTerms, IdentityBinding, ProviderRef,
};
use korpis_core::authz::{
    Action, Conditions, Delegation, Grant, GrantRecord, Kind, ObjectRef, Scope, Via,
};
use korpis_core::principal::Principal;
use std::collections::BTreeSet;
use std::time::Duration;
use uuid::Uuid;

/// The admins set, bound and verified, due an hour from now.
async fn admins(f: &Fixture) -> IdentityBinding {
    let provider = ProviderRef { organization: f.organization, name: "corp".into() };
    let set = Principal::ExternalSet {
        provider: provider.key(),
        realm: "groups".into(),
        member_of: "admins".into(),
    };
    let b = IdentityBinding {
        id: Uuid::now_v7(),
        subject: set,
        provider,
        origin: BindingOrigin::Operator,
        terms: BindingTerms::default(),
        state: BindingState::Verified,
        last_verified_at: Some(now()),
        verifications: 0,
        version: 1,
        made_by: alice(),
        made_at: now(),
        retired_at: None,
    };
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_binding(&b).await.unwrap();
    let due = now() + Duration::from_secs(3600);
    tx.record_verification((b.id, 0), BindingState::Verified, b.last_verified_at, due)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    b
}

/// A grant `member` issued through `set`, beneath the fixture's root grant.
fn delegated(f: &Fixture, set: &Principal, member: &Principal) -> GrantRecord {
    let at = now();
    let helper = Principal::Token(Uuid::now_v7());
    GrantRecord {
        grant: Grant {
            id: Uuid::now_v7(),
            issued_at: at,
            via: Some(Via { set: set.subject(), member: member.subject() }),
            subject: helper.subject(),
            actions: BTreeSet::from([Action::parse("org.read").unwrap()]),
            scope: Scope::Object(ObjectRef::new(Kind::Organization, f.organization)),
            conditions: Conditions::default(),
            parent: Some(f.grant),
            delegation: Delegation::default(),
            link_form: false,
            uses: 0,
            revoked: false,
            suspended: false,
            irreversible_override: false,
            root: false,
        },
        holder: helper,
        issued_by: member.clone(),
        issued_at: at,
        revoked_at: None,
        suspended_at: None,
        label: String::new(),
        from_template: None,
    }
}

#[tokio::test]
async fn a_member_who_delegated_is_recorded_asked_about_and_answered() {
    let f = fixture().await;
    let b = admins(&f).await;
    let ann = Principal::ExternalUser { provider: b.provider.key(), user: "ann".into() };
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_grant(&delegated(&f, &b.subject, &ann)).await.unwrap();
    tx.insert_grant(&delegated(&f, &b.subject, &ann)).await.unwrap();
    tx.commit().await.unwrap();
    let recorded = f.store.member(b.id, &ann.key()).await.unwrap().unwrap();
    assert_eq!((recorded.state, recorded.last_verified_at), (BindingState::Unverified, None));
    let asked = f.store.members_who_delegated(b.id).await.unwrap();
    let asked: Vec<String> = asked.iter().map(|m| m.member.key()).collect();
    assert_eq!(asked, [ann.key()], "once");
    let mut tx = f.store.begin().await.unwrap();
    let due = tx.claim_due_bindings(now(), now() + Duration::from_secs(30), 8).await.unwrap();
    assert_eq!(due.iter().map(|d| d.id).collect::<Vec<_>>(), [b.id], "due at once, not in an hour");

    let at = now();
    tx.record_members(b.id, (&[(ann.key(), 0)], &[]), at).await.unwrap();
    tx.commit().await.unwrap();
    let confirmed = f.store.member(b.id, &ann.key()).await.unwrap().unwrap();
    assert_eq!((confirmed.state, confirmed.last_verified_at), (BindingState::Verified, Some(at)));
    let mut tx = f.store.begin().await.unwrap();
    tx.record_members(b.id, (&[], &[(ann.key(), 1)]), now()).await.unwrap();
    tx.commit().await.unwrap();
    let left = f.store.member(b.id, &ann.key()).await.unwrap().unwrap();
    assert_eq!((left.state, left.last_verified_at), (BindingState::Unverified, Some(at)));
    assert_eq!(left.answers, 2, "each answer recorded counts");
}

#[tokio::test]
async fn an_answer_asked_before_a_newer_record_says_only_that_a_member_is_gone() {
    let f = fixture().await;
    let b = admins(&f).await;
    let ann = Principal::ExternalUser { provider: b.provider.key(), user: "ann".into() };
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_grant(&delegated(&f, &b.subject, &ann)).await.unwrap();
    tx.commit().await.unwrap();
    // Asked about while her record had no answers; a sign-in no longer naming the set writes it.
    let asked = f.store.members_who_delegated(b.id).await.unwrap()[0].answers;
    let mut tx = f.store.begin().await.unwrap();
    tx.record_sign_in(&ann.key(), &b.provider, &[], now()).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = f.store.begin().await.unwrap();
    let stale = tx.record_members(b.id, (&[(ann.key(), asked)], &[]), now()).await.unwrap();
    tx.commit().await.unwrap();
    let kept = f.store.member(b.id, &ann.key()).await.unwrap().unwrap();
    assert_eq!((kept.state, kept.last_verified_at), (BindingState::Unverified, None));
    assert_eq!((stale, kept.answers), (0, 1), "the older answer that she is there is dropped");

    let mut tx = f.store.begin().await.unwrap();
    let stale = tx.record_members(b.id, (&[], &[(ann.key(), asked)]), now()).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(stale, 1, "that she is gone is recorded, and said to be older than her record");
    assert_eq!(f.store.member(b.id, &ann.key()).await.unwrap().unwrap().answers, 2);
}
