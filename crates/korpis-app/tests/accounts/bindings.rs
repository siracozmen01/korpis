// SPDX-License-Identifier: AGPL-3.0-or-later

//! An identity binding read as evaluation finds it (§5.1 of docs/08-identity.md): one whose
//! last confirmation, interval, and grace have passed reads unverified before any
//! re-verification records it, so a reader is never shown a binding as vouching when it does
//! not (Finding 118 of docs/23-walkthroughs.md).

use crate::support::fixture;
use korpis_app::Page;
use korpis_app::ports::{BindingRead, BindingWrite, Store, Tx};
use korpis_core::authz::binding::{
    BindingOrigin, BindingState, BindingTerms, IdentityBinding, ProviderRef,
};
use korpis_core::principal::Principal;
use std::time::SystemTime;
use uuid::Uuid;

#[tokio::test]
async fn a_confirmation_past_its_interval_and_grace_reads_unverified() {
    let f = fixture().await;
    let provider = ProviderRef { organization: f.organization, name: "corp".into() };
    let now = SystemTime::now();
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
        state: BindingState::Verified,
        last_verified_at: Some(now),
        verifications: 1,
        version: 1,
        made_by: f.root.principal.clone(),
        made_at: now,
        retired_at: None,
    };
    let store = f.db.store().await.unwrap();
    let mut tx = store.begin().await.unwrap();
    tx.insert_binding(&b).await.unwrap();
    tx.commit().await.unwrap();
    let read = f.app.get_identity_binding(&f.root, b.id).await.unwrap();
    assert_eq!(read.state, BindingState::Verified, "within its interval");

    f.clock.advance(b.terms.reverify_interval + b.terms.grace);
    f.clock.advance(std::time::Duration::from_secs(2));
    let read = f.app.get_identity_binding(&f.root, b.id).await.unwrap();
    assert_eq!(read.state, BindingState::Unverified, "lapsed, though nothing recorded it");
    let listed = f.app.list_identity_bindings(&f.root, f.organization, false, Page::default());
    let listed = listed.await.unwrap().items;
    assert_eq!(listed.iter().map(|b| b.state).collect::<Vec<_>>(), [BindingState::Unverified]);
    let stored = store.binding(b.id).await.unwrap().unwrap();
    assert_eq!(stored.state, BindingState::Verified, "a read records nothing");
}
