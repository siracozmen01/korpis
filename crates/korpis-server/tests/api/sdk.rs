// SPDX-License-Identifier: AGPL-3.0-or-later

//! A test extension written with the SDK (`korpis-extension`), against a real control plane: it
//! registers as the DNS provider its installation provides and is pinged through the API, and it
//! consumes the events its installation subscribes to, acknowledging each once handled.

use crate::extensions::{DNS, push_document};
use crate::providers::installed_from;
use crate::recipes::project;
use crate::support::Harness;
use korpis_extension::consumer::{Consumer, EventHandler};
use korpis_extension::provider::{Provider, Registration, Unsupported};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;

/// Passes every event it is handed to the test.
struct Forward(mpsc::UnboundedSender<pb::Event>);

impl EventHandler for Forward {
    fn event(&self, event: pb::Event) -> impl Future<Output = Result<(), String>> + Send {
        std::future::ready(self.0.send(event).map_err(|e| e.to_string()))
    }
}

/// How long an event the test expects may take: the shared test server's horizon.
const DELIVERY: Duration = Duration::from_secs(60);

#[tokio::test]
async fn an_extension_written_with_the_sdk_serves_its_provider_and_consumes_its_events() {
    let h = Harness::new().await;
    // The DNS extension, subscribed to project events so the test can make some.
    let document = DNS.replace("kinds: [endpoint.]", "kinds: [project.]");
    let x = installed_from(&h, 41, "dns", push_document(&document).await).await;
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
    })));

    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_DNS,
        name: "cloudflare".into(),
        version: "1.0.0".into(),
        concurrency: 4,
    };
    let provider = Arc::new(Provider::new(Arc::clone(&connection), registration, Unsupported));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    tokio::time::timeout(Duration::from_secs(10), registered.wait_for(Option::is_some))
        .await
        .expect("registered within ten seconds")
        .unwrap();
    let ping = pb::PingProviderRequest {
        organization: h.root_org(),
        r#type: pb::ProviderType::PROVIDER_TYPE_DNS.into(),
        name: "cloudflare".into(),
        ..Default::default()
    };
    let pinged = pb::ProviderServiceClient::new(h.http.clone(), h.root())
        .ping_provider(ping)
        .await
        .unwrap()
        .into_owned();
    assert!(pinged.error.as_option().is_none(), "the SDK answers a ping: {pinged:?}");
    assert_eq!(pinged.version, "1.0.0");

    let (tx, mut rx) = mpsc::unbounded_channel();
    let consumer = Arc::new(Consumer::new(Arc::clone(&connection), Forward(tx)));
    let subscriptions = consumer.subscriptions().await.unwrap();
    assert_eq!(subscriptions.len(), 1, "the one its document declares");
    let (following, subscription) = (Arc::clone(&consumer), subscriptions[0].clone());
    tokio::spawn(async move { following.run(&subscription).await });
    project(&h, &h.root_org(), "web").await;
    let event = tokio::time::timeout(DELIVERY, rx.recv()).await.unwrap().unwrap();
    assert_eq!(event.kind, "project.create");

    // It acknowledged what it handled, so its position moved to it or past it.
    let mut acked = String::new();
    for _ in 0..50 {
        let (http, config) = connection.client().await.unwrap();
        let read =
            pb::GetSubscriptionRequest { id: subscriptions[0].clone(), ..Default::default() };
        let s = pb::EventServiceClient::new(http, config).get_subscription(read).await.unwrap();
        acked = s.into_owned().subscription.into_option().unwrap().acked;
        // Positions are fixed-width, so their identifiers order as they do; later events of the
        // kind may have been handled and acknowledged too.
        if acked.len() == event.id.len() && acked >= event.id {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("acknowledged {acked:?}, not {}", event.id);
}
