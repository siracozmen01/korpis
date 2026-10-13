// SPDX-License-Identifier: AGPL-3.0-or-later

//! A relay's certificate kept current (§7 of docs/18-operations.md): the relay renews it with a
//! fresh key and serves the new one at once, and a certificate left unrenewed is reported as it
//! nears expiry, once per step, until a renewal clears it.

use crate::agent::TestAgent;
use crate::standalone::joined;
use crate::support::Harness;
use connectrpc::ErrorCode;
use korpis_proto::korpis::relay::v1 as relay;
use korpis_server::standalone::feed::signed_renewal;
use korpis_server::standalone::renew::renew_once;
use korpis_server::standalone::run;
use korpis_server::standalone::served::{Served, load};
use std::time::{Duration, SystemTime};

#[tokio::test]
async fn a_relay_renews_its_certificate_and_serves_the_new_one_at_once() {
    let h = Harness::new().await;
    let (_dir, relay_dir, url, listener) = joined(&h).await;
    let id = relay_dir.joined().unwrap().unwrap();
    let running = run(relay_dir.clone(), listener).await.unwrap();
    let before = running.served.chain();
    let expires = renew_once(&relay_dir, &id, &running.served).await.unwrap();
    assert_ne!(running.served.chain()[0], before[0], "a new certificate, served");
    assert!(expires > SystemTime::now() + Duration::from_secs(80 * 24 * 60 * 60));
    assert_eq!(relay_dir.tls_generation().unwrap(), 1);
    assert!(!relay_dir.tls_files(0).key.exists(), "the generation before is forgotten");
    assert_eq!(h.effects("relay.certificate_renew").await.len(), 1, "issuing is on the record");

    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let _attached = crate::standalone::admitted(&h, &a, &url).await;

    let control = relay::RelayControlServiceClient::new(h.http.clone(), h.config(None));
    let key = rcgen::KeyPair::generate().unwrap();
    let mut swapped =
        signed_renewal(&id, &relay_dir.identity().unwrap(), key.public_key_raw()).unwrap();
    swapped.tls_key = rcgen::KeyPair::generate().unwrap().public_key_raw().to_vec();
    let refused = control.renew(swapped).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unauthenticated, "the signature vouches for the key");
    running.task.abort();
}

#[tokio::test]
async fn an_unrenewed_relay_certificate_is_reported_once_per_step() {
    let h = Harness::new().await;
    let (_dir, relay_dir, _, _) = joined(&h).await;
    let id = relay_dir.joined().unwrap().unwrap();
    let store = h.db.store().await.unwrap();
    let expire_in = |days: i32| {
        sqlx::query("UPDATE relay SET certificate_expires_at = now() + make_interval(days => $1)")
            .bind(days)
            .execute(store.pool())
    };
    let app = h.api.app();
    assert_eq!(app.alert_relay_certificates().await.unwrap(), 0, "ninety days left");
    expire_in(10).await.unwrap();
    assert_eq!(app.alert_relay_certificates().await.unwrap(), 1);
    assert_eq!(app.alert_relay_certificates().await.unwrap(), 0, "once per step");
    expire_in(2).await.unwrap();
    assert_eq!(app.alert_relay_certificates().await.unwrap(), 1);
    let reported: Vec<String> = sqlx::query_scalar(
        "SELECT severity FROM event WHERE kind = 'relay.certificate_expiring' ORDER BY seq",
    )
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert_eq!(reported, ["warning", "critical"]);

    let served = Served::new(load(&relay_dir.tls_files(0)).unwrap());
    renew_once(&relay_dir, &id, &served).await.unwrap();
    expire_in(10).await.unwrap();
    assert_eq!(
        app.alert_relay_certificates().await.unwrap(),
        1,
        "a renewal starts the count again"
    );
}
