// SPDX-License-Identifier: AGPL-3.0-or-later

//! Event subscriptions (§5 of docs/15-observability.md): a subscription is its subject's own
//! stored position, starting at the end of the log; an extension's are created with its
//! installation and end with it (§4 of docs/16-extensions.md).

use crate::extensions::{install, needs, push_dns};
use crate::recipes::project;
use crate::support::{Harness, detail, mutation};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

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
