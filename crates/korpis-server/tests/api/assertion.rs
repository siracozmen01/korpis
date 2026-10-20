// SPDX-License-Identifier: AGPL-3.0-or-later

//! Asserting an external identity (§5.2 of docs/08-identity.md), against a real control plane
//! and an identity provider written with the SDK: a client holding `identity.relay` over the
//! provider's organization passes on what a person presented, the provider says who presented
//! it, and the client is handed a ticket for the one action on the one object. A presentation
//! is accepted once, the person's binding is made at their first use, and a client without the
//! action, or evidence the provider refuses, gets nothing.

use crate::idp::serving;
use crate::support::Harness;
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::external_identity::Selector;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// The person presenting `evidence` asks to read the root organization.
fn asked(h: &Harness, evidence: &str) -> pb::AssertIdentityRequest {
    let object = pb::ObjectRef {
        kind: pb::ObjectKind::OBJECT_KIND_ORGANIZATION.into(),
        id: h.root_org(),
        ..Default::default()
    };
    pb::AssertIdentityRequest {
        organization: h.root_org(),
        provider: "corp".into(),
        evidence: evidence.as_bytes().to_vec(),
        action: "org.read".into(),
        object: Some(object).into(),
        ..Default::default()
    }
}

async fn bindings(h: &Harness) -> Vec<pb::IdentityBinding> {
    let req = pb::ListIdentityBindingsRequest { organization: h.root_org(), ..Default::default() };
    let listed = h.identity(h.root()).list_identity_bindings(req).await.unwrap();
    listed.into_owned().bindings
}

#[tokio::test]
async fn a_relay_is_handed_one_ticket_per_presentation_and_the_person_is_bound_at_first_use() {
    let h = Harness::new().await;
    let (_running, secret) = serving(&h, 53, &Arc::new(AtomicBool::new(true))).await;
    let relay = h.identity(h.config(Some(&secret)));
    let asserted = relay.assert_identity(asked(&h, "ann:p1")).await.unwrap().into_owned();
    assert!(asserted.ticket.starts_with("korpisas_"), "a ticket, shown once");
    let subject = asserted.subject.as_option().unwrap();
    assert_eq!(subject.provider, format!("{}/corp", h.root_org()));
    assert_eq!(subject.selector, Some(Selector::User("ann".into())));
    assert_eq!(asserted.sets.len(), 1, "the sets the provider says she is in");
    assert_eq!(asserted.sets[0].member_of, "admins");
    assert!(asserted.expires_at.is_set(), "{asserted:?}");

    let made = bindings(&h).await;
    assert_eq!(made.len(), 1, "her binding, made at her first use");
    let first = &made[0];
    assert_eq!(first.created_by.as_known(), Some(pb::BindingOrigin::BINDING_ORIGIN_FIRST_USE));
    assert_eq!(first.state.as_known(), Some(pb::BindingState::BINDING_STATE_VERIFIED));
    assert_eq!(first.subject.as_option(), Some(subject));
    let effects = h.effects("identity.asserted").await;
    assert_eq!(effects.len(), 1, "under the relay's own grant: {effects:?}");
    assert!(effects[0].1.is_some());

    let replayed = relay.assert_identity(asked(&h, "ann:p1")).await.unwrap_err();
    assert_eq!(replayed.code, ErrorCode::Aborted, "a presentation is accepted once");
    let again = relay.assert_identity(asked(&h, "ann:p2")).await.unwrap().into_owned();
    assert_ne!(again.ticket, asserted.ticket);
    assert_eq!(bindings(&h).await.len(), 1, "her binding is made once");
}

#[tokio::test]
async fn nothing_is_handed_without_the_action_or_for_evidence_the_provider_refuses() {
    let h = Harness::new().await;
    let (_running, secret) = serving(&h, 54, &Arc::new(AtomicBool::new(true))).await;
    let (token, reader) = h.bare_token().await;
    h.issue_to(token, "reader", &["org.read"]).await;
    let reader = h.identity(h.config(Some(reader.expose())));
    let e = reader.assert_identity(asked(&h, "ann:p1")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "reading is not relaying");
    let relay = h.identity(h.config(Some(&secret)));
    let nobody = relay.assert_identity(asked(&h, "eve:p1")).await.unwrap_err();
    assert_eq!(nobody.code, ErrorCode::InvalidArgument, "the provider's word: {nobody:?}");
    let stale = relay.assert_identity(asked(&h, "ann:p2:600")).await.unwrap_err();
    assert_eq!(stale.code, ErrorCode::InvalidArgument, "presented ten minutes ago");
    assert!(bindings(&h).await.is_empty(), "no one is bound by a refusal");

    let mut elsewhere = asked(&h, "ann:p3");
    elsewhere.provider = "absent".into();
    let e = relay.assert_identity(elsewhere).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable, "no such provider is asked: {e:?}");
}
