// SPDX-License-Identifier: AGPL-3.0-or-later

//! An identity provider written with the SDK, against a real control plane (§5.1 of
//! docs/08-identity.md): it registers as the `identity` provider its installation provides, and
//! a binding it vouches for is re-verified by asking it over its own stream. A set it still has
//! is verified; one it no longer has is unverified at once; and while it cannot be asked the
//! binding stays as it was, its grace not yet spent, with the reason said.

use crate::bindings::{bind, identity_document, set};
use crate::extensions::push_document;
use crate::providers::installed_from;
use crate::support::Harness;
use korpis_extension::identity_provider::{Answer, Asked, Asserted, Failure, Identity};
use korpis_extension::identity_provider::{IdentityProvider, Set};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

/// A directory with one set, `groups/admins`, while `present` says it exists.
struct Directory {
    present: Arc<AtomicBool>,
}

impl IdentityProvider for Directory {
    fn verify(&self, asked: Asked) -> impl Future<Output = Result<Answer, Failure>> + Send {
        let admins = Set { realm: "groups".into(), member_of: "admins".into() };
        let exists = self.present.load(Ordering::SeqCst) && asked.set == Some(admins);
        std::future::ready(Ok(Answer { set_exists: exists, present: Vec::new() }))
    }

    fn assert(
        &self,
        _: Vec<u8>,
        _: BTreeMap<String, String>,
    ) -> impl Future<Output = Result<Asserted, Failure>> + Send {
        std::future::ready(Err(Failure {
            reason: "identity.unsupported".into(),
            message: String::new(),
            retryable: false,
        }))
    }
}

async fn verify(h: &Harness, id: &str) -> pb::VerifyIdentityBindingResponse {
    let req = pb::VerifyIdentityBindingRequest { id: id.to_owned(), ..Default::default() };
    h.identity(h.root()).verify_identity_binding(req).await.unwrap().into_owned()
}

fn state(r: &pb::VerifyIdentityBindingResponse) -> Option<pb::BindingState> {
    r.binding.as_option().and_then(|b| b.state.as_known())
}

/// The directory, installed as the `n`th node's extension, registered and serving; while the
/// handle runs.
async fn serving(h: &Harness, n: u8, present: &Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
    let repo = push_document(&identity_document()).await;
    let x = installed_from(h, n, &format!("idp-{n}"), repo).await;
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
        relay: None,
    })));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_IDENTITY,
        name: "corp".into(),
        version: "1.0.0".into(),
        concurrency: 4,
    };
    let directory = Identity(Directory { present: Arc::clone(present) });
    let provider = Arc::new(Provider::new(connection, registration, directory));
    let mut registered = provider.registered();
    let running = tokio::spawn(async move {
        let _ = provider.run().await;
        drop(x);
    });
    tokio::time::timeout(Duration::from_secs(10), registered.wait_for(Option::is_some))
        .await
        .expect("registered within ten seconds")
        .unwrap();
    running
}

#[tokio::test]
async fn a_binding_is_verified_by_asking_its_own_provider_and_fails_closed_without_it() {
    let h = Harness::new().await;
    let present = Arc::new(AtomicBool::new(true));
    let running = serving(&h, 51, &present).await;
    let org = h.root_org();
    let made = bind(&h, &org, set(&org, "admins"), pb::IdentityBindingTerms::default());
    let made = made.await.unwrap();
    let confirmed = verify(&h, &made.id).await;
    assert_eq!(state(&confirmed), Some(pb::BindingState::BINDING_STATE_VERIFIED), "{confirmed:?}");
    assert!(confirmed.unanswered.is_empty());
    let first = confirmed.binding.as_option().unwrap().last_verified_at.as_option().cloned();
    assert!(first.is_some(), "dated");
    assert_eq!(h.effects("identity.binding.verified").await.len(), 1);

    present.store(false, Ordering::SeqCst);
    let ended = verify(&h, &made.id).await;
    assert_eq!(state(&ended), Some(pb::BindingState::BINDING_STATE_UNVERIFIED), "at once");
    present.store(true, Ordering::SeqCst);
    let back = verify(&h, &made.id).await;
    assert_eq!(state(&back), Some(pb::BindingState::BINDING_STATE_VERIFIED), "reversible");

    running.abort();
    let unanswered = verify(&h, &made.id).await;
    assert!(!unanswered.unanswered.is_empty(), "the reason is said: {unanswered:?}");
    assert_eq!(
        state(&unanswered),
        Some(pb::BindingState::BINDING_STATE_VERIFIED),
        "an outage is carried for the binding's grace"
    );
}

#[tokio::test]
async fn a_binding_is_verified_by_itself_as_soon_as_it_is_made() {
    let h = Harness::new().await;
    let present = Arc::new(AtomicBool::new(true));
    let _running = serving(&h, 52, &present).await;
    let org = h.root_org();
    let made = bind(&h, &org, set(&org, "admins"), pb::IdentityBindingTerms::default());
    let made = made.await.unwrap();
    let identity = h.identity(h.root());
    let get = pb::GetIdentityBindingRequest { id: made.id.clone(), ..Default::default() };
    for _ in 0..60 {
        let read = identity.get_identity_binding(get.clone()).await.unwrap().into_owned();
        let binding = read.binding.as_option().unwrap();
        if binding.state.as_known() == Some(pb::BindingState::BINDING_STATE_VERIFIED) {
            let effects = h.effects("identity.binding.verified").await;
            assert_eq!(effects.len(), 1, "by the installation's authority, under no grant");
            assert_eq!(effects[0].1, None);
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("the binding was never re-verified by the control plane's own loop");
}
