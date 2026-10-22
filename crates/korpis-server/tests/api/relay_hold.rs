// SPDX-License-Identifier: AGPL-3.0-or-later

//! Relays a restore holds (§6 of docs/18-operations.md): every relay the backup records as joined
//! is held, its feed answered as unavailable and its URL named to no node, until an operator
//! readmits it or removes it; a relay the store does not know is refused as unauthenticated.

use crate::relays::{Feed, token};
use crate::support::{Harness, mutation};
use connectrpc::ErrorCode;
use korpis_app::ports::SystemClock;
use korpis_app::{App, NoRecipes};
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::relay::v1 as relay;
use korpis_proto::korpis::v1 as pb;
use korpis_server::standalone::feed::signed_request;
use korpis_server::standalone::{RelayDir, join};
use relay::__buffa::oneof::feed_update::Message as Update;
use std::time::Duration;

/// How opening the feed with a fresh request ends: `None` when it opens and sends the token keys.
async fn opened(h: &Harness, dir: &RelayDir, id: &str) -> Option<ErrorCode> {
    let control = relay::RelayControlServiceClient::new(h.http.clone(), h.config(None));
    let request = signed_request(id, &dir.identity().unwrap(), &None).unwrap();
    let mut feed: Feed = match control.feed(request).await {
        Ok(feed) => feed,
        Err(e) => return Some(e.code),
    };
    let keys = |u: relay::FeedUpdate| matches!(u.message, Some(Update::TokenKeys(_)));
    let wait = async {
        loop {
            match feed.message::<relay::FeedUpdate>().await {
                Err(e) => return Some(e.code),
                Ok(None) => return Some(ErrorCode::Unknown),
                Ok(Some(u)) if keys(u.to_owned_message()) => return None,
                Ok(Some(_)) => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), wait).await.expect("within ten seconds")
}

async fn readmit(h: &Harness, id: &str, key: &str) -> Result<pb::Relay, ErrorCode> {
    let r = h
        .nodes(h.root())
        .readmit_relay(pb::ReadmitRelayRequest {
            mutation: mutation(key).into(),
            organization: h.root_org(),
            relay: id.to_owned(),
            ..Default::default()
        })
        .await;
    r.map(|r| r.into_owned().relay.into_option().unwrap()).map_err(|e| e.code)
}

async fn listed(h: &Harness) -> pb::Relay {
    let request = pb::ListRelaysRequest { organization: h.root_org(), ..Default::default() };
    let relays = h.nodes(h.root()).list_relays(request).await.unwrap().into_owned().relays;
    relays.into_iter().next().unwrap()
}

#[tokio::test]
async fn a_held_relay_is_fed_nothing_and_named_to_no_node_until_it_is_readmitted() {
    let h = Harness::new().await;
    let created = token(&h, &h.root_org(), "edge-1").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = RelayDir::new(tmp.path().join("relay"));
    let id = join(&dir, &h.base_url(), &created.token, &created.ca_fingerprint).await.unwrap();
    let control = relay::RelayControlServiceClient::new(h.http.clone(), h.config(None));
    let mut live: Feed =
        control.feed(signed_request(&id, &dir.identity().unwrap(), &None).unwrap()).await.unwrap();
    let app = App::new(h.db.store().await.unwrap(), SystemClock, test_key(), NoRecipes);
    assert_eq!(app.relay_urls().await.unwrap(), ["https://localhost:9443"]);

    assert_eq!(app.hold_relays("uid 0 (root)").await.unwrap(), 1);
    assert_eq!(app.hold_relays("uid 0 (root)").await.unwrap(), 0, "held once");
    assert_eq!(h.effects("restore.relays.held").await.len(), 2);
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        while live.message::<relay::FeedUpdate>().await.ok().flatten().is_some() {}
    });
    ended.await.expect("a feed open when the relay is held ends");
    assert_eq!(opened(&h, &dir, &id).await, Some(ErrorCode::Unavailable), "asked again later");
    assert!(app.relay_urls().await.unwrap().is_empty(), "named to no node");
    let held = listed(&h).await;
    assert!(held.held_at.as_option().is_some() && !held.connected);

    let readmitted = readmit(&h, &id, "readmit").await.unwrap();
    assert!(readmitted.held_at.as_option().is_none());
    assert_eq!(readmit(&h, &id, "again").await.unwrap_err(), ErrorCode::Aborted, "not held");
    assert_eq!(h.effects("relay.readmit").await.len(), 1);
    assert_eq!(opened(&h, &dir, &id).await, None, "fed again");
    assert_eq!(app.relay_urls().await.unwrap(), ["https://localhost:9443"]);

    app.hold_relays("uid 0 (root)").await.unwrap();
    h.nodes(h.root())
        .remove_relay(pb::RemoveRelayRequest {
            mutation: mutation("remove").into(),
            organization: h.root_org(),
            relay: id.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(readmit(&h, &id, "removed").await.unwrap_err(), ErrorCode::Aborted);
    assert_eq!(opened(&h, &dir, &id).await, Some(ErrorCode::Unauthenticated), "it stops");
    assert_eq!(app.hold_relays("uid 0 (root)").await.unwrap(), 0, "a removed relay is not held");

    // A relay that joined after the backup is not in the restored store: unknown, refused.
    let unknown = Id::generate(IdKind::Relay).to_string();
    assert_eq!(opened(&h, &dir, &unknown).await, Some(ErrorCode::Unauthenticated));
}

fn test_key() -> std::sync::Arc<korpis_core::authz::token::SigningKey> {
    std::sync::Arc::new(korpis_core::authz::token::SigningKey::generate(9))
}
