// SPDX-License-Identifier: AGPL-3.0-or-later

//! A session held by a person signed in through an identity provider (§9.7 of
//! docs/08-identity.md, migration 0101): kept with the sets of the sign-in, read and ended by
//! its holder alone, and unusable once the person's binding is retired; and the person recorded
//! in the sets their sign-in named, and asked about by those sets while their session lives.

use crate::support::{fixture, now};
use korpis_app::ports::{
    BindingWrite, MemberRead, MemberWrite, Presented, Store, Tx, UserSessionRead, UserSessionWrite,
};
use korpis_core::account::session::{Holder, SessionKind, UserSession};
use korpis_core::authz::binding::{
    BindingOrigin, BindingState, BindingTerms, IdentityBinding, ProviderRef,
};
use korpis_core::principal::{Principal, SetName};
use std::time::Duration;
use uuid::Uuid;

fn person(organization: Uuid) -> IdentityBinding {
    let provider = ProviderRef { organization, name: "corp".into() };
    let subject = Principal::ExternalUser { provider: provider.key(), user: "dana".into() };
    IdentityBinding {
        id: Uuid::now_v7(),
        provider,
        made_by: subject.clone(),
        subject,
        origin: BindingOrigin::FirstUse,
        terms: BindingTerms::default(),
        state: BindingState::Verified,
        last_verified_at: Some(now()),
        verifications: 0,
        version: 1,
        made_at: now(),
        retired_at: None,
    }
}

fn session(holder: Holder) -> UserSession {
    UserSession {
        id: Uuid::now_v7(),
        holder,
        sets: vec![SetName { realm: "groups".into(), member_of: "ops".into() }],
        kind: SessionKind::Web,
        created_at: now(),
        last_used_at: now(),
        authenticated_at: now(),
        expires_at: now() + Duration::from_secs(3600),
        user_agent: "test".into(),
        source: "203.0.113.7".into(),
        portal: None,
    }
}

#[tokio::test]
async fn a_person_s_session_keeps_its_sets_and_answers_only_to_its_holder() {
    let f = fixture().await;
    let b = person(f.organization);
    let held = Holder::Person(b.id);
    let s = session(held);
    let (first, second) = ([1u8; 32], [2u8; 32]);
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_binding(&b).await.unwrap();
    tx.insert_user_session(&s, &first).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        f.store.user_sessions(held).await.unwrap(),
        std::slice::from_ref(&s),
        "with its sets"
    );
    let stranger = Holder::Account(Uuid::now_v7());
    assert!(f.store.user_sessions(stranger).await.unwrap().is_empty());
    assert_eq!(f.store.user_session(stranger, s.id).await.unwrap(), None);

    let mut tx = f.store.begin().await.unwrap();
    let Presented::Current { session: read, usable } = tx.present_refresh(&first).await.unwrap()
    else {
        panic!("its current credential");
    };
    assert_eq!((read, usable), (s.clone(), true));
    tx.rotate_refresh(s.id, &first, &second, now()).await.unwrap();
    let rotated = tx.present_refresh(&first).await.unwrap();
    assert_eq!(rotated, Presented::Rotated { session: s.id, holder: held }, "a copy, of hers");
    assert!(tx.retire_binding(b.id, now()).await.unwrap());
    let Presented::Current { usable, .. } = tx.present_refresh(&second).await.unwrap() else {
        panic!("still her session");
    };
    assert!(!usable, "a retired binding signs nobody in");
    assert!(!tx.end_user_session(stranger, s.id).await.unwrap(), "only its holder ends it");
    assert!(tx.end_user_session(held, s.id).await.unwrap());
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn a_sign_in_records_its_person_in_its_sets_and_their_session_has_them_asked_about() {
    let f = fixture().await;
    let dana = person(f.organization);
    let mut ops = person(f.organization);
    ops.id = Uuid::now_v7();
    ops.subject = Principal::ExternalSet {
        provider: ops.provider.key(),
        realm: "groups".into(),
        member_of: "ops".into(),
    };
    ops.origin = BindingOrigin::Operator;
    let s = session(Holder::Person(dana.id));
    let (member, at) = (dana.subject.key(), now());
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_binding(&dana).await.unwrap();
    tx.insert_binding(&ops).await.unwrap();
    tx.record_sign_in(&member, &dana.provider, &[ops.id], at).await.unwrap();
    tx.insert_user_session(&s, &[3u8; 32]).await.unwrap();
    tx.commit().await.unwrap();

    let record = f.store.member(ops.id, &member).await.unwrap().expect("recorded at sign-in");
    assert_eq!((record.state, record.last_verified_at), (BindingState::Verified, Some(at)));
    let asked = f.store.members_who_delegated(ops.id).await.unwrap();
    assert_eq!(
        asked,
        std::slice::from_ref(&record),
        "her live session has her asked about, not a grant"
    );

    // A sign-in naming the set no longer unverifies her record there; her session ended, she is
    // asked about no more.
    let mut tx = f.store.begin().await.unwrap();
    tx.record_sign_in(&member, &dana.provider, &[], now()).await.unwrap();
    assert!(tx.end_user_session(Holder::Person(dana.id), s.id).await.unwrap());
    tx.commit().await.unwrap();
    let record = f.store.member(ops.id, &member).await.unwrap().unwrap();
    assert_eq!(record.state, BindingState::Unverified);
    assert!(f.store.members_who_delegated(ops.id).await.unwrap().is_empty());
}
