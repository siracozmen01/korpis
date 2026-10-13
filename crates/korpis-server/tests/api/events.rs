// SPDX-License-Identifier: AGPL-3.0-or-later

//! Event subscriptions (§5 of docs/15-observability.md): a subscription is its subject's own
//! stored position, starting at the end of the log; an extension's are created with its
//! installation and end with it (§4 of docs/16-extensions.md).

use crate::extensions::{install, needs, push_dns};
use crate::recipes::project;
use crate::support::{Harness, detail, mutation};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::subscribe_response::Item;
use std::time::Duration;

fn create(key: &str, kinds: &[&str]) -> pb::CreateSubscriptionRequest {
    pb::CreateSubscriptionRequest {
        mutation: mutation(key).into(),
        filter: pb::EventFilter {
            kinds: kinds.iter().map(|k| (*k).to_owned()).collect(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_subscription_is_its_subjects_own_and_is_acknowledged_only_up_to_the_log() {
    let h = Harness::new().await;
    let events = h.events(h.root());
    let e = events.create_subscription(create("bad", &["Workload"])).await.unwrap_err();
    assert_eq!(detail(&e).field, "filter.kinds");
    let made = events.create_subscription(create("s", &["workload."])).await.unwrap();
    let s = made.into_owned().subscription.into_option().unwrap();
    assert_eq!(s.filter.as_option().unwrap().kinds, ["workload."]);
    let listed = events.list_subscriptions(pb::ListSubscriptionsRequest::default()).await;
    assert_eq!(listed.unwrap().into_owned().subscriptions.len(), 1);

    let (_, secret) = h.bare_token().await;
    let other = h.events(h.config(Some(secret.expose())));
    let get = pb::GetSubscriptionRequest { id: s.id.clone(), ..Default::default() };
    let e = other.get_subscription(get.clone()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "another subject's subscription is not even a name");

    let far = korpis_wire::event::event_id(1 << 40);
    let ack = |through: &str| pb::AckRequest {
        subscription: s.id.clone(),
        through: through.to_owned(),
        ..Default::default()
    };
    let e = events.ack(ack(&far)).await.unwrap_err();
    assert_eq!(detail(&e).field, "through", "nothing past what the log has positioned");

    let delete = pb::DeleteSubscriptionRequest {
        mutation: mutation("d").into(),
        id: s.id.clone(),
        ..Default::default()
    };
    events.delete_subscription(delete).await.unwrap();
    assert_eq!(events.get_subscription(get).await.unwrap_err().code, ErrorCode::NotFound);
}

#[tokio::test]
async fn an_extensions_subscriptions_come_and_go_with_its_installation() {
    let h = Harness::new().await;
    let repo = push_dns().await;
    let project = project(&h, &h.root_org(), "dns").await;
    let approved = needs(&h, &repo, &project).await;
    let req = pb::InstallExtensionRequest {
        approved_needs: approved,
        ..install(&h, "install", &repo, &project)
    };
    let done = h.extensions(h.root()).install_extension(req).await.unwrap().into_owned();
    let id = done.extension.into_option().unwrap().id;
    let ext = korpis_core::id::Id::parse(korpis_core::id::IdKind::Extension, &id).unwrap().uuid();
    let store = h.db.store().await.unwrap();
    let count = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM event_subscription
              WHERE extension_id = $1 AND subject_key = 'extension:' || $1",
        )
        .bind(ext)
        .fetch_one(store.pool())
        .await
        .unwrap()
    };
    assert_eq!(count().await, 1, "one subscription per subscribes entry, held by the extension");

    let uninstall =
        pb::UninstallExtensionRequest { mutation: mutation("u").into(), id, ..Default::default() };
    h.extensions(h.root()).uninstall_extension(uninstall).await.unwrap();
    assert_eq!(count().await, 0, "they end with the installation");
}

type Delivery = connectrpc::client::ServerStream<
    <connectrpc::client::HttpClient as connectrpc::client::ClientTransport>::ResponseBody,
    pb::__buffa::view::SubscribeResponseView<'static>,
>;

async fn open(h: &Harness, config: connectrpc::client::ClientConfig, s: &str) -> Delivery {
    let req = pb::SubscribeRequest { subscription: s.to_owned(), ..Default::default() };
    h.events(config).subscribe(req).await.unwrap()
}

/// How long a test waits for an event it expects. Delivery waits for every transaction on the
/// PostgreSQL server that could still commit an earlier event (§5 of docs/15-observability.md),
/// and the test server is shared by every test running at once, whose teardowns drop databases
/// in transactions that last seconds while they wait for checkpoints; so an expected event may
/// come many seconds late, and is never lost.
const DELIVERY: u64 = 60;

/// The next event delivered, within `seconds`; `None` if none comes.
async fn next(stream: &mut Delivery, seconds: u64) -> Option<pb::Event> {
    let got = tokio::time::timeout(Duration::from_secs(seconds), stream.message()).await.ok()?;
    let item = got.unwrap().expect("the stream is open").to_owned_message().item;
    match item {
        Some(Item::Event(e)) => Some(*e),
        other => panic!("not an event: {other:?}"),
    }
}

async fn subscribe(h: &Harness, config: connectrpc::client::ClientConfig, key: &str) -> String {
    let made = h.events(config).create_subscription(create(key, &["project."])).await.unwrap();
    made.into_owned().subscription.into_option().unwrap().id
}

#[tokio::test]
async fn delivery_is_in_order_judged_by_the_subjects_grants_and_resumes_after_its_ack() {
    let h = Harness::new().await;
    let s = subscribe(&h, h.root(), "s").await;
    let mut stream = open(&h, h.root(), &s).await;
    project(&h, &h.root_org(), "one").await;
    project(&h, &h.root_org(), "two").await;
    let first = next(&mut stream, DELIVERY).await.expect("the first project");
    let second = next(&mut stream, DELIVERY).await.expect("the second project");
    assert_eq!((first.kind.as_str(), second.kind.as_str()), ("project.create", "project.create"));
    assert_eq!(first.key, second.key, "both about the organization");
    assert_eq!(second.seq, first.seq + 1, "numbered in commit order, without gaps");
    assert!(first.id < second.id);

    let ack = pb::AckRequest { subscription: s.clone(), through: first.id, ..Default::default() };
    h.events(h.root()).ack(ack).await.unwrap();
    drop(stream);
    let mut again = open(&h, h.root(), &s).await;
    let redelivered = next(&mut again, DELIVERY).await.expect("what was not acknowledged");
    assert_eq!(redelivered.id, second.id, "at least once, resuming after the acknowledgement");

    // A subject without event.read is delivered nothing. Once it holds the grant, what it was
    // never delivered is delivered on its next connection: judged when sent, not when it
    // happened, and never acknowledged in between.
    let (token, secret) = h.bare_token().await;
    let config = || h.config(Some(secret.expose()));
    let theirs = subscribe(&h, config(), "t").await;
    let mut blind = open(&h, config(), &theirs).await;
    project(&h, &h.root_org(), "three").await;
    assert!(next(&mut blind, 3).await.is_none(), "it may not read the organization's events");
    drop(blind);
    h.issue_to(token, "read", &["event.read"]).await;
    let mut seeing = open(&h, config(), &theirs).await;
    let seen = next(&mut seeing, DELIVERY).await.expect("delivered once it may read it");
    let name = seen.detail.get("name").and_then(|v| match &v.kind {
        Some(pb::__buffa::oneof::value::Kind::StringValue(s)) => Some(s.as_str()),
        _ => None,
    });
    assert_eq!(name, Some("three"));
}

/// The next item of any kind, within ten seconds.
async fn item(stream: &mut Delivery) -> Item {
    let got = tokio::time::timeout(Duration::from_secs(10), stream.message()).await;
    got.expect("an item").unwrap().expect("the stream is open").to_owned_message().item.unwrap()
}

#[tokio::test]
async fn a_position_lost_to_retention_is_answered_with_a_gap_and_delivery_goes_on() {
    let h = Harness::new().await;
    let s = subscribe(&h, h.root(), "s").await;
    project(&h, &h.root_org(), "one").await;
    project(&h, &h.root_org(), "two").await;
    let store = h.db.store().await.unwrap();
    // Wait until both are positioned, then age every event past retention and prune.
    let mut stream = open(&h, h.root(), &s).await;
    next(&mut stream, DELIVERY).await.unwrap();
    next(&mut stream, DELIVERY).await.unwrap();
    drop(stream);
    sqlx::query("UPDATE event SET at = now() - interval '40 days'")
        .execute(store.pool())
        .await
        .unwrap();
    assert!(store.prune_events(Duration::from_secs(30 * 86_400)).await.unwrap() >= 2);

    let mut stream = open(&h, h.root(), &s).await;
    let Item::Gap(gap) = item(&mut stream).await else { panic!("the loss is said first") };
    assert_eq!(gap.reason, "retention");
    project(&h, &h.root_org(), "three").await;
    let Item::Event(e) = item(&mut stream).await else { panic!("then delivery goes on") };
    assert!(e.id >= gap.resumes_at);
}
