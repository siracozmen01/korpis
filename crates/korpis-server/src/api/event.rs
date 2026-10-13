// SPDX-License-Identifier: AGPL-3.0-or-later

//! `EventService` (§5 of docs/15-observability.md): the caller's subscriptions, stored positions
//! in the event log, their delivery, and their acknowledgements.

use super::wire::event::{
    event_id, event_to_pb, filter_from_pb, position_from, subscription_to_pb,
};
use super::wire::{self, Refuse};
use super::{Api, ServerApp, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{
    ConnectError, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream,
};
use korpis_app::event::{Delivered, Delivery};
use korpis_app::{Caller, Idempotent};
use korpis_core::event::Subscription;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::subscribe_response::Item;
use std::sync::Arc;

fn item(d: Delivered) -> pb::SubscribeResponse {
    let item = match d {
        Delivered::Event(e) => Item::Event(Box::new(event_to_pb(&e))),
        Delivered::Gap { resumes_at } => Item::Gap(Box::new(pb::EventGap {
            resumes_at: event_id(resumes_at),
            reason: "retention".into(),
            ..Default::default()
        })),
    };
    pb::SubscribeResponse { item: Some(item), ..Default::default() }
}

/// The stream of a delivery, one batch read from the log each time the client has taken the
/// last, so it reads at the client's pace.
fn delivered(
    app: Arc<ServerApp>,
    caller: Caller,
    delivery: Delivery<korpis_store::EventPositions>,
) -> impl futures_util::Stream<Item = Result<pb::SubscribeResponse, ConnectError>> + Send + use<> {
    let batches = futures_util::stream::unfold(Some((app, caller, delivery)), |state| async move {
        let (app, caller, mut delivery) = state?;
        match app.next_delivery(&caller, &mut delivery).await {
            Ok(Some(batch)) => {
                let items: Vec<_> = batch.into_iter().map(|d| Ok(item(d))).collect();
                Some((futures_util::stream::iter(items), Some((app, caller, delivery))))
            }
            Ok(None) => None,
            Err(e) => Some((futures_util::stream::iter(vec![Err(error::from_app(e))]), None)),
        }
    });
    futures_util::StreamExt::flatten(batches)
}

fn created(s: &Subscription) -> pb::CreateSubscriptionResponse {
    pb::CreateSubscriptionResponse {
        subscription: subscription_to_pb(s).into(),
        ..Default::default()
    }
}

impl pb::EventService for Api {
    async fn create_subscription(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateSubscriptionRequest>,
    ) -> ServiceResult<pb::CreateSubscriptionResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let filter = req.filter.as_option().cloned().unwrap_or_default();
        let filter = filter_from_pb("filter", &filter).refuse()?;
        let encoder = |s: &Subscription| encode(&created(s));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.create_subscription(&caller, &idem, filter).await.map_err(error::from_app)?;
        respond(reply, created)
    }

    async fn get_subscription(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetSubscriptionRequest>,
    ) -> ServiceResult<pb::GetSubscriptionResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Subscription, "id", request.id).refuse()?;
        let s = self.app.get_subscription(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetSubscriptionResponse {
            subscription: subscription_to_pb(&s).into(),
            ..Default::default()
        })
    }

    async fn list_subscriptions(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListSubscriptionsRequest>,
    ) -> ServiceResult<pb::ListSubscriptionsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Subscription)?;
        let listed = self.app.list_subscriptions(&caller, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListSubscriptionsResponse {
            subscriptions: listed.items.iter().map(subscription_to_pb).collect(),
            page: next_page(listed.next, IdKind::Subscription).into(),
            ..Default::default()
        })
    }

    async fn delete_subscription(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeleteSubscriptionRequest>,
    ) -> ServiceResult<pb::DeleteSubscriptionResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Subscription, "id", &req.id).refuse()?;
        let encoder = |(): &()| encode(&pb::DeleteSubscriptionResponse::default());
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.delete_subscription(&caller, &idem, id).await.map_err(error::from_app)?;
        respond(reply, |()| pb::DeleteSubscriptionResponse::default())
    }

    async fn subscribe(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SubscribeRequest>,
    ) -> ServiceResult<ServiceStream<pb::SubscribeResponse>> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Subscription, "subscription", request.subscription).refuse()?;
        let delivery = self.app.subscribe(&caller, id).await.map_err(error::from_app)?;
        Response::stream_ok(delivered(Arc::clone(&self.app), caller, delivery))
    }

    async fn ack(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::AckRequest>,
    ) -> ServiceResult<pb::AckResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Subscription, "subscription", request.subscription).refuse()?;
        let through = position_from("through", request.through).refuse()?;
        let acked =
            self.app.ack_subscription(&caller, id, through).await.map_err(error::from_app)?;
        Response::ok(pb::AckResponse { acked: event_id(acked), ..Default::default() })
    }
}
