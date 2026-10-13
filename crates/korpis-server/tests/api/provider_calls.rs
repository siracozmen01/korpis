// SPDX-License-Identifier: AGPL-3.0-or-later

//! Provider calls (§3.3 of docs/16-extensions.md): a call is queued, sent down a stream of the
//! newest registered version with a slot free, and finished by the provider's answer, by the
//! stream ending, or by its deadline, when the provider is told to stop.

use crate::providers::{Serve, installed, open, register};
use crate::support::Harness;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::call_result::Outcome;
use korpis_proto::korpis::v1::__buffa::oneof::provider_control::Message as Down;
use korpis_proto::korpis::v1::__buffa::oneof::provider_message::Message as Up;
use std::time::Duration;
use tokio::task::JoinHandle;

/// Pings `name` as the root organization sees it, in the background, waiting at most `timeout`.
fn ping(h: &Harness, name: &str, timeout: Duration) -> JoinHandle<pb::PingProviderResponse> {
    let client = pb::ProviderServiceClient::new(h.http.clone(), h.root());
    let req = pb::PingProviderRequest {
        organization: h.root_org(),
        r#type: pb::ProviderType::PROVIDER_TYPE_DNS.into(),
        name: name.to_owned(),
        timeout: korpis_wire::duration(timeout).into(),
        ..Default::default()
    };
    tokio::spawn(async move { client.ping_provider(req).await.unwrap().into_owned() })
}

/// The next message down a provider's stream, if one comes within `within`.
async fn next(stream: &mut Serve, within: Duration) -> Option<Down> {
    let m = tokio::time::timeout(within, stream.message::<pb::ProviderControl>()).await.ok()?;
    m.unwrap().map(|m| m.to_owned_message().message).unwrap()
}

/// The call that comes down next, within five seconds.
async fn call(stream: &mut Serve) -> Box<pb::ProviderCall> {
    match next(stream, Duration::from_secs(5)).await {
        Some(Down::Call(c)) => c,
        other => panic!("expected a call, got {other:?}"),
    }
}

/// Answers a ping with its nonce.
async fn pong(stream: &mut Serve, c: &pb::ProviderCall) {
    let Some(korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request::Ping(p)) =
        &c.request
    else {
        panic!("a ping");
    };
    let answer = pb::Pong { nonce: p.nonce.clone(), ..Default::default() };
    let r = pb::CallResult {
        call: c.call.clone(),
        outcome: Some(Outcome::Pong(Box::new(answer))),
        ..Default::default()
    };
    let up = pb::ProviderMessage { message: Some(Up::Result(Box::new(r))), ..Default::default() };
    stream.send(up).await.unwrap();
}

async fn registered(h: &Harness, secret: &str, version: &str, concurrency: u32) -> (Serve, String) {
    let (stream, first) =
        open(h, h.config(Some(secret)), register("cloudflare", version, concurrency)).await;
    let Some(Down::Registered(r)) = first.unwrap().message else { panic!("Registered") };
    (stream, r.registration.clone())
}

fn reason(r: &pb::PingProviderResponse) -> &str {
    r.error.as_option().map_or("", |e| e.reason.as_str())
}

#[tokio::test]
async fn a_call_goes_down_the_stream_and_its_answer_comes_back() {
    let h = Harness::new().await;
    let x = installed(&h, 21, "dns").await;
    let (mut stream, registration) = registered(&h, &x.secret, "1.0.0", 4).await;
    let pinging = ping(&h, "cloudflare", Duration::from_secs(10));
    let c = call(&mut stream).await;
    assert!(c.call.starts_with("pcl_"), "{}", c.call);
    pong(&mut stream, &c).await;
    let answered = pinging.await.unwrap();
    assert_eq!(reason(&answered), "", "{answered:?}");
    assert_eq!((answered.registration, answered.version), (registration, "1.0.0".to_owned()));
    assert_eq!(answered.call, c.call);

    let listed = pb::ProviderServiceClient::new(h.http.clone(), h.root())
        .list_providers(pb::ListProvidersRequest {
            organization: h.root_org(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(listed.providers.len(), 1);
    assert_eq!(listed.providers[0].extension, x.extension.id);
}

#[tokio::test]
async fn an_unanswered_call_fails_at_its_deadline_and_the_provider_is_told_to_stop() {
    let h = Harness::new().await;
    let x = installed(&h, 22, "dns").await;
    let (mut stream, _) = registered(&h, &x.secret, "1.0.0", 4).await;
    let pinging = ping(&h, "cloudflare", Duration::from_secs(1));
    let c = call(&mut stream).await;
    let late = pinging.await.unwrap();
    assert_eq!(reason(&late), "provider.deadline");
    assert!(late.error.as_option().unwrap().retryable);
    match next(&mut stream, Duration::from_secs(5)).await {
        Some(Down::Cancel(cancel)) => assert_eq!(cancel.call, c.call),
        other => panic!("expected a cancellation, got {other:?}"),
    }
    pong(&mut stream, &c).await; // Too late: recorded nowhere, and harmless.
}

#[tokio::test]
async fn a_call_in_flight_fails_as_retryable_when_its_stream_ends() {
    let h = Harness::new().await;
    let x = installed(&h, 23, "dns").await;
    let (mut stream, _) = registered(&h, &x.secret, "1.0.0", 4).await;
    let pinging = ping(&h, "cloudflare", Duration::from_secs(60));
    call(&mut stream).await;
    drop(stream);
    let ended = tokio::time::timeout(Duration::from_secs(10), pinging).await.unwrap().unwrap();
    assert_eq!(reason(&ended), "provider.stream_ended");
    assert!(ended.error.as_option().unwrap().retryable);
}

#[tokio::test]
async fn new_calls_go_to_the_newest_version_while_the_old_one_drains() {
    let h = Harness::new().await;
    let x = installed(&h, 24, "dns").await;
    let (mut old, _) = registered(&h, &x.secret, "1.0.0", 4).await;
    let draining = ping(&h, "cloudflare", Duration::from_secs(10));
    let in_flight = call(&mut old).await;
    let (mut new, newest) = registered(&h, &x.secret, "1.1.0", 4).await;
    let fresh = ping(&h, "cloudflare", Duration::from_secs(10));
    let c = call(&mut new).await;
    pong(&mut new, &c).await;
    let answered = fresh.await.unwrap();
    assert_eq!((answered.registration, answered.version), (newest, "1.1.0".to_owned()));
    assert!(next(&mut old, Duration::from_millis(300)).await.is_none(), "the old one drains");
    pong(&mut old, &in_flight).await;
    assert_eq!(draining.await.unwrap().version, "1.0.0", "what it held, it still answers");
}

#[tokio::test]
async fn a_stream_is_never_sent_more_calls_than_it_accepts_at_once() {
    let h = Harness::new().await;
    let x = installed(&h, 25, "dns").await;
    let (mut stream, _) = registered(&h, &x.secret, "1.0.0", 1).await;
    let first = ping(&h, "cloudflare", Duration::from_secs(10));
    let second = ping(&h, "cloudflare", Duration::from_secs(10));
    let c1 = call(&mut stream).await;
    assert!(next(&mut stream, Duration::from_millis(500)).await.is_none(), "one at a time");
    pong(&mut stream, &c1).await;
    let c2 = call(&mut stream).await;
    pong(&mut stream, &c2).await;
    assert_eq!(reason(&first.await.unwrap()), "");
    assert_eq!(reason(&second.await.unwrap()), "");
}

#[tokio::test]
async fn a_provider_nothing_provides_is_unknown_at_once() {
    let h = Harness::new().await;
    let unknown = ping(&h, "route53", Duration::from_secs(10)).await.unwrap();
    assert_eq!(reason(&unknown), "provider.unknown");
    assert!(!unknown.error.as_option().unwrap().retryable);
    assert!(unknown.registration.is_empty());
}
