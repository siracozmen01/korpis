// SPDX-License-Identifier: AGPL-3.0-or-later

//! A person signed in through an identity provider, calling (§9.7 of docs/08-identity.md): their
//! access token names their session, which the gateway reads at each call; they hold the sets
//! of the sign-in only while their record as a member of each vouches, no second factor is
//! theirs, and a retired binding or an ended session authenticates nobody.

use crate::support::{Fixture, fixture};
use korpis_app::AppError;
use korpis_app::ports::{BindingWrite, Clock as _, Store, Tx, UserSessionWrite};
use korpis_core::account::session::{Holder, SessionKind, UserSession};
use korpis_core::authz::access::{ACCESS_LIFETIME, AccessClaims, mint};
use korpis_core::authz::binding::{
    BindingOrigin, BindingState, BindingTerms, IdentityBinding, ProviderRef,
};
use korpis_core::principal::{Principal, SetName};
use std::time::Duration;
use uuid::Uuid;

fn bound(f: &Fixture, subject: Principal) -> IdentityBinding {
    let now = f.clock.now();
    IdentityBinding {
        id: Uuid::now_v7(),
        provider: ProviderRef { organization: f.organization, name: "corp".into() },
        made_by: subject.clone(),
        subject,
        origin: BindingOrigin::Operator,
        terms: BindingTerms::default(),
        state: BindingState::Verified,
        last_verified_at: Some(now),
        verifications: 0,
        version: 1,
        made_at: now,
        retired_at: None,
    }
}

fn set(name: &str) -> SetName {
    SetName { realm: "groups".into(), member_of: name.into() }
}

/// Dana's binding and session, with the sets `ops`, which is bound and holds a record of her,
/// and `oncall`, which is not bound; and the token of her session minted now.
async fn signed_in(f: &Fixture) -> (IdentityBinding, Principal, Uuid) {
    let provider = ProviderRef { organization: f.organization, name: "corp".into() }.key();
    let dana = Principal::ExternalUser { provider: provider.clone(), user: "dana".into() };
    let ops = Principal::ExternalSet { provider, realm: "groups".into(), member_of: "ops".into() };
    let (person, group) = (bound(f, dana), bound(f, ops.clone()));
    let now = f.clock.now();
    let session = UserSession {
        id: Uuid::now_v7(),
        holder: Holder::Person(person.id),
        sets: vec![set("ops"), set("oncall")],
        kind: SessionKind::Web,
        created_at: now,
        last_used_at: now,
        authenticated_at: now,
        expires_at: now + Duration::from_secs(3600),
        user_agent: "test".into(),
        source: "203.0.113.7".into(),
    };
    let store = f.db.store().await.unwrap();
    let mut tx = store.begin().await.unwrap();
    tx.insert_binding(&person).await.unwrap();
    tx.insert_binding(&group).await.unwrap();
    tx.insert_user_session(&session, &[7u8; 32]).await.unwrap();
    tx.commit().await.unwrap();
    sqlx::query(
        "INSERT INTO identity_member (binding_id, member, state, last_verified_at, recorded_at)
         VALUES ($1, $2, 'verified', now(), now())",
    )
    .bind(group.id)
    .bind(person.subject.key())
    .execute(store.pool())
    .await
    .unwrap();
    (person, ops, session.id)
}

fn token(f: &Fixture, person: &IdentityBinding, session: Uuid) -> String {
    let holder = Holder::Person(person.id);
    let expires_at = f.clock.now() + ACCESS_LIFETIME;
    mint(&f.key, &AccessClaims { holder, session, expires_at })
}

#[tokio::test]
async fn a_person_holds_the_sets_of_their_sign_in_only_while_their_records_vouch() {
    let f = fixture().await;
    let (person, ops, session) = signed_in(&f).await;
    let caller = f.app.authenticate(&token(&f, &person, session), None).await.unwrap();
    assert_eq!(caller.principal, person.subject, "the person, not an account");
    assert_eq!(caller.memberships, [ops.subject()], "ops vouches; oncall has no binding");
    assert!(!caller.mfa, "no second factor is theirs");
    assert_eq!((caller.session, caller.holder), (Some(session), Some(Holder::Person(person.id))));

    // Past the set's interval and grace with no confirmation, her record no longer vouches.
    f.clock.advance(Duration::from_secs(31 * 60));
    let caller = f.app.authenticate(&token(&f, &person, session), None).await.unwrap();
    assert!(caller.memberships.is_empty(), "a lapsed record holds no set");

    let store = f.db.store().await.unwrap();
    let mut tx = store.begin().await.unwrap();
    tx.retire_binding(person.id, f.clock.now()).await.unwrap();
    tx.commit().await.unwrap();
    let refused = f.app.authenticate(&token(&f, &person, session), None).await;
    assert!(matches!(refused, Err(AppError::Unauthenticated)), "a retired binding: {refused:?}");
}
