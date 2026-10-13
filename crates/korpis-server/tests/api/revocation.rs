// SPDX-License-Identifier: AGPL-3.0-or-later

//! The revocation list down the agent session (§6.3 of docs/08-identity.md): sent when a session
//! opens, sent again when a grant is revoked, and signed by the token key the node was given at
//! enrollment.

use crate::agent::{Stream, TestAgent, next_list, welcome};
use crate::support::{Harness, mutation};
use korpis_core::authz::revocation::RevocationList;
use korpis_core::authz::token::{KeyRing, parse_key_name};
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::v1 as pb;
use korpis_wire::revocation::revocation_from_pb;
use std::time::SystemTime;

async fn open(h: &Harness, a: &TestAgent, nonce: u8) -> Stream {
    let (stream, first) = TestAgent::open(h, a.hello_at(SystemTime::now(), &[nonce; 16])).await;
    welcome(&first.unwrap());
    stream
}

async fn verified(a: &TestAgent, stream: &mut Stream) -> RevocationList {
    let mut ring = KeyRing::default();
    for k in &a.token_keys {
        ring.insert(parse_key_name(&k.key_id).unwrap(), &k.public_key).unwrap();
    }
    revocation_from_pb(&next_list(stream).await).unwrap().verify(&ring).expect("signed")
}

#[tokio::test]
async fn every_session_is_sent_the_signed_list_and_every_change_to_it() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let mut stream = open(&h, &a, 1).await;
    let opened = verified(&a, &mut stream).await;
    assert!(opened.grants.is_empty(), "nothing is revoked yet");

    let (token, _) = h.bare_token().await;
    let grant = h.issue_to(token, "g1", &["org.read"]).await;
    h.grants(h.root())
        .revoke_grant(pb::RevokeGrantRequest {
            mutation: mutation("r1").into(),
            grant: grant.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let grant = Id::parse(IdKind::Grant, &grant).unwrap().uuid();
    let changed = verified(&a, &mut stream).await;
    assert_eq!(changed.grants, [grant]);
    assert!(changed.version > opened.version);

    let mut again = open(&h, &a, 2).await;
    assert_eq!(verified(&a, &mut again).await, changed, "a new session is sent the current list");
}
