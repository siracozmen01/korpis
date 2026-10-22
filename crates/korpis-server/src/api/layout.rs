// SPDX-License-Identifier: AGPL-3.0-or-later

//! `OrganizationService.GetLayout`, `GetLandingLayout`, and `SetLayout` (question 2 of §10 of
//! docs/11-surface-web.md).

use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::tenancy::{GoverningLayout, SetLayout};
use korpis_core::id::IdKind;
use korpis_core::tenancy::layout::{Asked, AskedItem, Item, Layout};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::layout_item::Kind as K;

fn item_to_pb(i: &Item) -> pb::LayoutItem {
    let kind = match i {
        Item::Places => K::Places(Box::default()),
        Item::Quota => K::Quota(Box::default()),
        Item::Notice { heading, text } => K::Notice(Box::new(pb::LayoutNotice {
            heading: heading.clone(),
            text: text.clone(),
            ..Default::default()
        })),
        Item::Link { label, url } => K::Link(Box::new(pb::LayoutLink {
            label: label.clone(),
            url: url.clone(),
            ..Default::default()
        })),
    };
    pb::LayoutItem { kind: Some(kind), ..Default::default() }
}

fn layout_to_pb(l: &Layout) -> pb::Layout {
    pb::Layout {
        name: l.name.clone(),
        items: l.items.iter().map(item_to_pb).collect(),
        ..Default::default()
    }
}

/// An item as the wire asked for it, to be checked by the application.
fn item_from_pb(i: usize, item: &pb::LayoutItem) -> Result<AskedItem, connectrpc::ConnectError> {
    Ok(match &item.kind {
        Some(K::Places(_)) => AskedItem::Places,
        Some(K::Quota(_)) => AskedItem::Quota,
        Some(K::Notice(n)) => {
            AskedItem::Notice { heading: n.heading.clone(), text: n.text.clone() }
        }
        Some(K::Link(l)) => AskedItem::Link { label: l.label.clone(), url: l.url.clone() },
        Some(K::Slot(name)) => AskedItem::Slot(name.clone()),
        None => {
            let field = format!("layout.items[{i}]");
            return Err(error::invalid(&field, "an item is one of the panel's components"));
        }
    })
}

fn asked_from_pb(l: &pb::Layout) -> Result<Asked, connectrpc::ConnectError> {
    let items = l.items.iter().enumerate().map(|(i, item)| item_from_pb(i, item));
    Ok(Asked { name: l.name.clone(), items: items.collect::<Result<_, _>>()? })
}

fn governing_to_pb(g: &GoverningLayout) -> pb::GoverningLayout {
    let set = g.set.as_ref();
    pb::GoverningLayout {
        layout: set.map(|s| layout_to_pb(&s.layout)).into(),
        set_on: set.map(|s| wire::wire_id(IdKind::Organization, s.organization)),
        own_version: g.own_version,
        ..Default::default()
    }
}

fn set_response(g: &GoverningLayout) -> pb::SetLayoutResponse {
    pb::SetLayoutResponse { governing: governing_to_pb(g).into(), ..Default::default() }
}

/// The layout that applies to the object.
pub(super) async fn get(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::GetLayoutRequest>,
) -> ServiceResult<pb::GetLayoutResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let of = req.of.as_option().cloned().unwrap_or_default();
    let object = korpis_wire::scope::object_from_pb("of", &of).refuse()?;
    let g = api.app.get_layout(&caller, object).await.map_err(error::from_app)?;
    Response::ok(pb::GetLayoutResponse {
        governing: governing_to_pb(&g).into(),
        ..Default::default()
    })
}

/// The layout of the address the caller reached the control plane at.
pub(super) async fn landing(
    api: &Api,
    ctx: &RequestContext,
) -> ServiceResult<pb::GetLandingLayoutResponse> {
    let caller = api.caller(ctx).await?;
    let portal = super::account::at(ctx).map(|p| p.portal);
    let g = api.app.landing_layout(&caller, portal).await.map_err(error::from_app)?;
    Response::ok(pb::GetLandingLayoutResponse {
        governing: governing_to_pb(&g).into(),
        ..Default::default()
    })
}

/// Sets or removes the organization's own layout.
pub(super) async fn set(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::SetLayoutRequest>,
) -> ServiceResult<pb::SetLayoutResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let expected_version = req.mutation.as_option().and_then(|m| m.expected_version).unwrap_or(0);
    let cmd = SetLayout {
        organization: wire::id(IdKind::Organization, "organization", &req.organization).refuse()?,
        layout: req.layout.as_option().map(asked_from_pb).transpose()?,
        expected_version,
    };
    let encoder = |g: &GoverningLayout| encode(&set_response(g));
    let idem = Idempotent { key, encode: &encoder };
    let reply = api.app.set_layout(&caller, &idem, cmd).await.map_err(error::from_app)?;
    respond(reply, set_response)
}
