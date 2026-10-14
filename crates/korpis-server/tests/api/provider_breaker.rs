// SPDX-License-Identifier: AGPL-3.0-or-later

//! A provider's circuit breaker (§5 of docs/16-extensions.md, finding 23 of
//! docs/23-walkthroughs.md): it trips on sustained failure and on sustained latency, refuses
//! calls at once while open, probes after its backoff, and each change is an event.

use crate::provider_calls::{call, next, ping, pong, reason, registered};
use crate::providers::{Serve, installed};
use crate::support::Harness;
use korpis_core::extension::FIRST_BACKOFF;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_control::Message as Down;
use std::time::Duration;

/// The provider events written so far: kind, severity, and the reason a trip names.
async fn provider_events(h: &Harness) -> Vec<(String, String, String)> {
    let store = h.db.store().await.unwrap();
    let rows: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT kind, severity, detail FROM event WHERE key LIKE 'provider:%' ORDER BY id",
    )
    .fetch_all(store.pool())
    .await
    .unwrap();
    rows.into_iter()
        .map(|(k, s, d)| (k, s, d["reason"].as_str().unwrap_or_default().to_owned()))
        .collect()
}

async fn health(h: &Harness) -> pb::ProviderHealth {
    let listed = pb::ProviderServiceClient::new(h.http.clone(), h.root())
        .list_providers(pb::ListProvidersRequest {
            organization: h.root_org(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    listed.health.into_iter().find(|p| p.name == "cloudflare").unwrap()
}

#[tokio::test]
async fn a_provider_that_always_answers_but_slowly_trips_the_breaker() {
    let h = Harness::new().await;
    let x = installed(&h, 31, "dns").await;
    let (mut stream, _) = registered(&h, &x.secret, "1.0.0", 10).await;
    let asked = tokio::time::Instant::now();
    let pings: Vec<_> = (0..10).map(|_| ping(&h, "cloudflare", Duration::from_secs(10))).collect();
    let mut calls = Vec::new();
    for _ in 0..10 {
        calls.push(call(&mut stream).await);
    }
    // Every call succeeds, at over four fifths of its deadline and a second and a half before it,
    // counted from when it was asked: delivery under load does not eat the margin.
    tokio::time::sleep_until(asked + Duration::from_millis(8500)).await;
    for c in &calls {
        pong(&mut stream, c).await;
    }
    for p in pings {
        assert_eq!(reason(&p.await.unwrap()), "", "each call itself succeeded");
    }
    assert_eq!(
        provider_events(&h).await,
        [("provider.degraded".to_owned(), "warning".to_owned(), "latency".to_owned())]
    );
    let refused = ping(&h, "cloudflare", Duration::from_secs(2)).await.unwrap();
    assert_eq!(reason(&refused), "provider.circuit_open");
    let e = refused.error.as_option().unwrap();
    assert!(e.retryable && e.retry_after.as_option().is_some(), "{e:?}");
    assert!(refused.registration.is_empty(), "a refused call is never sent");
    let shown = health(&h).await;
    assert_eq!(shown.breaker, pb::BreakerState::BREAKER_STATE_OPEN);
    assert!(shown.median_deadline_permille >= 800, "{shown:?}");
}

/// The next call down `stream` within half a second of the last message, past cancellations.
async fn next_call(stream: &mut Serve) -> Option<Box<pb::ProviderCall>> {
    loop {
        match next(stream, Duration::from_millis(500)).await {
            Some(Down::Call(c)) => return Some(c),
            Some(Down::Cancel(_)) => {}
            None => return None,
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_failing_provider_trips_then_a_probe_that_succeeds_closes_it() {
    let h = Harness::new().await;
    let x = installed(&h, 32, "dns").await;
    let (mut stream, _) = registered(&h, &x.secret, "1.0.0", 10).await;
    let pings: Vec<_> =
        (0..10).map(|_| ping(&h, "cloudflare", Duration::from_millis(500))).collect();
    for p in pings {
        assert_eq!(reason(&p.await.unwrap()), "provider.deadline");
    }
    let events = provider_events(&h).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].2, "failure");

    // The calls that went down before the trip, each followed by its cancellation; on a loaded
    // machine a call can reach its deadline before it is sent, and is then never sent.
    let mut seen = 0;
    loop {
        match next(&mut stream, Duration::from_secs(2)).await {
            Some(Down::Call(_)) => seen += 1,
            Some(Down::Cancel(_)) => {}
            None => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!((1..=10).contains(&seen), "{seen} calls went down");
    // Once its backoff passes, the next call is the probe, and it is answered at once. The trip
    // is recorded as the last deadline is, so the backoff is waited out by asking until a call
    // is let through rather than by a sleep that a loaded machine can outrun.
    tokio::time::sleep(FIRST_BACKOFF).await;
    let (probe, probing) = loop {
        let probing = ping(&h, "cloudflare", Duration::from_secs(5));
        // A late cancellation of one of the ten may still come first.
        let call = next_call(&mut stream).await;
        if let Some(c) = call {
            break (c, probing);
        }
        assert_eq!(reason(&probing.await.unwrap()), "provider.circuit_open", "refused, not lost");
    };
    pong(&mut stream, &probe).await;
    assert_eq!(reason(&probing.await.unwrap()), "");
    let kinds: Vec<_> = provider_events(&h).await.into_iter().map(|e| e.0).collect();
    assert_eq!(kinds, ["provider.degraded", "provider.recovered"]);
    assert_eq!(health(&h).await.breaker, pb::BreakerState::BREAKER_STATE_CLOSED);
}
