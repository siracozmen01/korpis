// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ExtensionService.ListSlots`: what installations contribute to a slot of the panel.

use super::wire::extension::action_to_pb;
use super::wire::{self, Refuse, wire_id};
use super::{Api, error};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::extension::SlotShown;
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;

/// The object a slot is shown for, by its identifier's prefix.
fn object(field: &str, id: &str) -> Result<ObjectRef, connectrpc::ConnectError> {
    if id.starts_with("org_") {
        let org = wire::id(IdKind::Organization, field, id).refuse()?;
        return Ok(ObjectRef::new(Kind::Organization, org));
    }
    Ok(ObjectRef::new(Kind::Workload, wire::id(IdKind::Workload, field, id).refuse()?))
}

fn contribution(s: &SlotShown) -> pb::SlotContribution {
    pb::SlotContribution {
        extension: wire_id(IdKind::Extension, s.installation.id),
        extension_name: s.installation.name.clone(),
        kind: s.widget.as_str().to_owned(),
        title: s.title.clone(),
        action: Some(action_to_pb(&s.action)).into(),
        ..Default::default()
    }
}

/// The contributions to a slot shown for an object.
pub(super) async fn list(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::ListSlotsRequest>,
) -> ServiceResult<pb::ListSlotsResponse> {
    let caller = api.caller(ctx).await?;
    let object = object("object", request.object)?;
    let shown =
        api.app.list_slots(&caller, request.slot, object).await.map_err(error::from_app)?;
    Response::ok(pb::ListSlotsResponse {
        contributions: shown.iter().map(contribution).collect(),
        ..Default::default()
    })
}
