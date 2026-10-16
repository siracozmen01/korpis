// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node removed as compromised held (§9 of docs/17-security.md): the certificate keys it
//! was given. An `acme` certificate whose key it held is given a new key at once, wherever its
//! endpoint is placed next, and the chain the node held is revoked through the directory; a
//! `provided` one says its key was exposed until its organization replaces it with a chain for
//! another key. Either is told to its organization as a `certificate.exposed` event.

use crate::agent::{Stream, TestAgent, welcome};
use crate::issuance::{placed, send, served_at, shown};
use crate::recipes::{project, push_paper};
use crate::routes::{attest, declare, web};
use crate::support::{Harness, mutation};
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::certificate_key::Outcome;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

/// The node named `name`, as the API names it.
async fn node_id(h: &Harness, name: &str) -> String {
    let store = h.db.store().await.unwrap();
    let id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM node WHERE name = $1")
        .bind(name)
        .fetch_one(store.pool())
        .await
        .unwrap();
    Id::from_uuid(IdKind::Node, id).to_string()
}

/// A second node serving the ingress, in session.
async fn spare(h: &Harness) -> Stream {
    let a = TestAgent::enroll(h, "fra-2", 2).await;
    let (s, first) = TestAgent::open(h, a.hello_at(SystemTime::now(), &[2; 16])).await;
    welcome(&first.unwrap());
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE node SET ingress = true").execute(store.pool()).await.unwrap();
    s
}

/// Asks for certificate `id` as a node serving it does, until it is given it.
async fn given(s: &mut Stream, id: &str) {
    let request = agent::CertificateRequest {
        request: "r".into(),
        certificate: id.into(),
        ..Default::default()
    };
    send(s, Up::CertificateRequest(Box::new(request))).await;
    for _ in 0..30 {
        let next = crate::agent::next(s).await;
        if let Some(Down::CertificateKey(k)) = next.message {
            assert!(matches!(k.outcome, Some(Outcome::Certificate(_))), "given");
            return;
        }
    }
    panic!("{id} was not given");
}

async fn remove_compromised(h: &Harness, name: &str) {
    let req = pb::RemoveNodeRequest {
        mutation: mutation("remove").into(),
        node: node_id(h, name).await,
        compromised: true,
        ..Default::default()
    };
    h.nodes(h.root()).remove_node(req).await.unwrap();
}

async fn count(h: &Harness, sql: &'static str) -> i64 {
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar(sql).fetch_one(store.pool()).await.unwrap()
}

#[tokio::test]
async fn an_acme_key_a_compromised_node_held_is_revoked_and_replaced() {
    let Some(pebble) = crate::pebble::start(true).await else { return };
    let h = Harness::with_acme(pebble.config(), None).await;
    let (mut lost, shop) = placed(&h).await;
    served_at(&mut lost, 1).await;
    let first = shown(&h, &shop.id).await.unwrap();
    given(&mut lost, &first.id).await;
    let mut kept = spare(&h).await;

    remove_compromised(&h, "fra-1").await;
    served_at(&mut kept, 2).await;
    let replaced = shown(&h, &shop.id).await.unwrap();
    assert_eq!((replaced.id.as_str(), replaced.revision), (first.id.as_str(), 2));
    assert!(!replaced.exposed, "a new key, not the one the node held");
    assert_eq!(count(&h, "SELECT count(*) FROM event WHERE kind = 'certificate.exposed'").await, 1);
    for _ in 0..100 {
        let revoked = "SELECT count(*) FROM certificate_exposure WHERE revocation = 'revoked'";
        if count(&h, revoked).await == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("the chain the node held was not revoked");
}

/// A self-signed chain for `names` under `key`, in PEM.
fn chain(names: &[&str], key: &rcgen::KeyPair) -> String {
    let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    rcgen::CertificateParams::new(names).unwrap().self_signed(key).unwrap().pem()
}

#[tokio::test]
async fn a_provided_key_a_compromised_node_held_is_exposed_until_replaced_by_another() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-1", 1).await;
    let (mut lost, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE node SET ingress = true").execute(store.pool()).await.unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let upload = pb::CreateCertificateRequest {
        mutation: mutation("c1").into(),
        organization: h.root_org(),
        chain: chain(&["shop.example.com"], &key),
        key: key.serialize_pem(),
        ..Default::default()
    };
    let made = h.certificates(h.root()).create_certificate(upload).await.unwrap().into_owned();
    let id = made.certificate.into_option().unwrap().id;
    let project = project(&h, &h.root_org(), "shops").await;
    attest(&h, "d1", &h.root_org(), "example.com").await;
    let mut body = web(&push_paper().await, &["shop.example.com"]);
    body.endpoints[0].tls = pb::TlsSpec {
        source: pb::TlsSource::TLS_SOURCE_PROVIDED.into(),
        secret: id.clone(),
        ..Default::default()
    }
    .into();
    let shop = declare(&h, &project, "shop", body).await;
    served_at(&mut lost, 1).await;
    given(&mut lost, &id).await;
    let _kept = spare(&h).await;

    remove_compromised(&h, "fra-1").await;
    assert!(shown(&h, &shop.id).await.unwrap().exposed, "its organization is told");
    let replace = |k: &str, chain: String, key: String| pb::ReplaceCertificateRequest {
        mutation: mutation(k).into(),
        id: id.clone(),
        chain,
        key,
        ..Default::default()
    };
    let same = replace("r1", chain(&["shop.example.com"], &key), key.serialize_pem());
    h.certificates(h.root()).replace_certificate(same).await.unwrap();
    assert!(shown(&h, &shop.id).await.unwrap().exposed, "a new chain for the same key is not");
    let other = rcgen::KeyPair::generate().unwrap();
    let fresh = replace("r2", chain(&["shop.example.com"], &other), other.serialize_pem());
    h.certificates(h.root()).replace_certificate(fresh).await.unwrap();
    assert!(!shown(&h, &shop.id).await.unwrap().exposed, "another key is no longer exposed");
}
