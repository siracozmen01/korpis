// SPDX-License-Identifier: AGPL-3.0-or-later

//! Presentations and tickets in the store (§5.2 of docs/08-identity.md): a presentation is
//! accepted once, a ticket works once, for the client it was handed to, before it expires, and
//! a person is bound at their first use once.

use crate::support::{Fixture, alice, fixture, now};
use korpis_app::ports::{AssertionWrite, BindingRead, BindingWrite, Store, Tx};
use korpis_core::authz::binding::{
    BindingOrigin, BindingState, BindingTerms, IdentityBinding, ProviderRef,
};
use korpis_core::authz::ticket::{TICKET_LIFETIME, Ticket};
use korpis_core::authz::{Action, Kind, ObjectRef};
use korpis_core::principal::{Principal, SetName};
use std::time::Duration;
use uuid::Uuid;

fn ticket(f: &Fixture) -> Ticket {
    let provider = ProviderRef { organization: f.organization, name: "corp".into() };
    Ticket {
        id: Uuid::now_v7(),
        relay: alice(),
        subject: Principal::ExternalUser { provider: provider.key(), user: "ann".into() },
        sets: vec![SetName { realm: "groups".into(), member_of: "admins".into() }],
        provider,
        presentation: "p1".into(),
        action: Action::parse("org.read").unwrap(),
        object: ObjectRef::new(Kind::Organization, f.organization),
        expires_at: now() + TICKET_LIFETIME,
    }
}

#[tokio::test]
async fn a_presentation_is_accepted_once_until_it_is_forgotten() {
    let f = fixture().await;
    let corp = ProviderRef { organization: f.organization, name: "corp".into() };
    let other = ProviderRef { organization: f.organization, name: "other".into() };
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.accept_presentation(&corp, "p1", now()).await.unwrap());
    assert!(!tx.accept_presentation(&corp, "p1", now()).await.unwrap(), "a replay");
    assert!(tx.accept_presentation(&other, "p1", now()).await.unwrap(), "each provider's own");
    let later = now() + Duration::from_secs(1);
    assert_eq!(tx.forget_assertions(later).await.unwrap(), 2);
    assert!(tx.accept_presentation(&corp, "p1", later).await.unwrap(), "forgotten");
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn a_ticket_works_once_for_its_relay_before_it_expires() {
    let f = fixture().await;
    let t = ticket(&f);
    let digest = [7u8; 32];
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_ticket(&t, &digest).await.unwrap();
    let stranger = Principal::User(Uuid::from_u128(2)).key();
    assert_eq!(tx.redeem_ticket(&digest, &stranger, now()).await.unwrap(), None, "not theirs");
    let late = t.expires_at + Duration::from_secs(1);
    assert_eq!(tx.redeem_ticket(&digest, &alice().key(), late).await.unwrap(), None, "expired");
    let used = tx.redeem_ticket(&digest, &alice().key(), now()).await.unwrap();
    assert_eq!(used.as_ref(), Some(&t), "read back as kept");
    let again = tx.redeem_ticket(&digest, &alice().key(), now()).await.unwrap();
    assert_eq!(again, None, "once");
    assert_eq!(tx.forget_assertions(late).await.unwrap(), 1, "expired tickets are forgotten");
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn a_person_is_bound_at_first_use_once_and_a_retirement_stands() {
    let f = fixture().await;
    let t = ticket(&f);
    let person = |at| IdentityBinding {
        id: Uuid::now_v7(),
        provider: t.provider.clone(),
        subject: t.subject.clone(),
        origin: BindingOrigin::FirstUse,
        terms: BindingTerms::default(),
        state: BindingState::Verified,
        last_verified_at: Some(at),
        verifications: 0,
        version: 1,
        made_by: t.subject.clone(),
        made_at: at,
        retired_at: None,
    };
    let first = person(now());
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.bind_at_first_use(&first).await.unwrap());
    assert!(!tx.bind_at_first_use(&person(now())).await.unwrap(), "bound already");
    tx.commit().await.unwrap();
    assert_eq!(f.store.binding_of(&t.subject.key()).await.unwrap(), Some(first.clone()));

    let mut tx = f.store.begin().await.unwrap();
    tx.retire_binding(first.id, now()).await.unwrap();
    assert!(!tx.bind_at_first_use(&person(now())).await.unwrap(), "an operator's decision");
    tx.commit().await.unwrap();
    assert_eq!(f.store.binding_of(&t.subject.key()).await.unwrap(), None);
}
