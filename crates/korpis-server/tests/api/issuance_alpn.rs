// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's proxy proving an endpoint's name by TLS-ALPN-01 (§3.2 of docs/07-networking.md),
//! against Pebble taking every challenge as valid. A certificate first proven over HTTP-01 is
//! recorded as proven so; one recorded as proven by TLS-ALPN-01, as a node whose port 80 is
//! closed leaves it, is renewed that way: its digest reaches the node in its assignment, beside
//! no HTTP-01 token, the directory is told to look only once the node holds it, and the way is
//! kept for the next renewal.

use crate::agent::Stream;
use crate::issuance::{placed, send, served_at};
use crate::journal::until;
use crate::support::Harness;
use buffa::Message as _;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use std::time::Duration;

/// The digest of the first set whose assignment carries a TLS-ALPN-01 challenge, and the
/// assignment.
fn alpn_challenged(d: &Down) -> Option<(String, agent::Assignment)> {
    let Down::AssignmentSet(s) = d else { return None };
    let a = agent::Assignment::decode_from_slice(&s.assignments.first()?.assignment).ok()?;
    (!a.alpn_challenges.is_empty()).then(|| (s.set_digest.clone(), a))
}

async fn node_proof(h: &Harness) -> Option<String> {
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar("SELECT node_proof FROM certificate").fetch_one(store.pool()).await.unwrap()
}

async fn renew_now(h: &Harness, way: &str) {
    let store = h.db.store().await.unwrap();
    sqlx::query(
        "UPDATE certificate SET node_proof = $1, renew_after = now() - interval '1 second'",
    )
    .bind(way)
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query("SELECT pg_notify('korpis_certificate', '')").execute(store.pool()).await.unwrap();
}

async fn ack(s: &mut Stream, digest: String) {
    send(s, Up::Ack(Box::new(agent::Ack { set_digest: digest, ..Default::default() }))).await;
}

#[tokio::test]
async fn a_certificate_a_node_proved_by_tls_alpn_is_renewed_the_same_way() {
    let Some(pebble) = crate::pebble::start(true).await else { return };
    let h = Harness::with_acme(pebble.config(), None).await;
    let (mut s, _) = placed(&h).await;
    served_at(&mut s, 1).await;
    assert_eq!(node_proof(&h).await.as_deref(), Some("http-01"), "never issued: HTTP-01 first");

    renew_now(&h, "tls-alpn-01").await;
    let (digest, a) = until(&mut s, alpn_challenged).await;
    assert!(a.challenges.is_empty(), "no HTTP-01 token beside it");
    let c = &a.alpn_challenges[0];
    assert_eq!((c.name.as_str(), c.digest.len()), ("shop.example.com", 32));
    // Nothing is issued while the node has not acknowledged the set carrying the digest.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let store = h.db.store().await.unwrap();
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM certificate")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(revision, 1, "not renewed before the node holds the digest");
    ack(&mut s, digest).await;
    served_at(&mut s, 2).await;
    assert_eq!(node_proof(&h).await.as_deref(), Some("tls-alpn-01"), "kept for the next renewal");
}
