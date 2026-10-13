// SPDX-License-Identifier: AGPL-3.0-or-later

//! `EventService` (§5 of docs/15-observability.md): the caller's subscriptions, stored positions
//! in the event log, and their acknowledgements.

use super::wire::event::{filter_from_pb, position_from, subscription_to_pb};
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream};
use korpis_app::{AppError, Idempotent};
use korpis_core::event::Subscription;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;

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
        _request: ServiceRequest<'_, pb::SubscribeRequest>,
    ) -> ServiceResult<ServiceStream<pb::SubscribeResponse>> {
        self.caller(&ctx).await?;
        Err(error::from_app(AppError::NotYet { what: "event delivery", phase: 1 }))
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
        Response::ok(pb::AckResponse { acked: wire::event::event_id(acked), ..Default::default() })
    }
}
