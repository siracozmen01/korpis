// SPDX-License-Identifier: AGPL-3.0-or-later

//! `IdentityService.AssertIdentity` (§5.2 of docs/08-identity.md).

use super::wire::scope::object_ref_from_pb;
use super::wire::{self, Refuse};
use super::{Api, error};
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::authz::AssertAsked;
use korpis_core::authz::Action;
use korpis_core::authz::binding::ProviderRef;
use korpis_core::authz::ticket::TicketObject;
use korpis_core::extension::identity::Evidence;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::identity::set_to_pb;

/// What a ticket is asked of: a grant, for revoking it, or an object of the hierarchy.
fn ticket_object(o: &pb::ObjectRef) -> Result<TicketObject, ConnectError> {
    if o.kind.as_known() == Some(pb::ObjectKind::OBJECT_KIND_GRANT) {
        return Ok(TicketObject::Grant(wire::id(IdKind::Grant, "object.id", &o.id).refuse()?));
    }
    Ok(TicketObject::Object(object_ref_from_pb("object", o).refuse()?))
}

impl Api {
    pub(super) async fn assert(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::AssertIdentityRequest>,
    ) -> ServiceResult<pb::AssertIdentityResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let organization =
            wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let object = req.object.as_option().ok_or_else(|| error::invalid("object", "required"))?;
        let asked = AssertAsked {
            provider: ProviderRef { organization, name: req.provider.clone() },
            evidence: Evidence {
                body: req.evidence.clone(),
                headers: req.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            },
            action: Action::parse(&req.action)
                .map_err(|e| error::invalid("action", e.to_string()))?,
            object: ticket_object(object)?,
        };
        let asserted = self.app.assert_identity(&caller, asked).await.map_err(error::from_app)?;
        let t = &asserted.ticket;
        let subject = match wire::principal::to_pb(&t.subject).kind {
            Some(pb::__buffa::oneof::subject::Kind::External(e)) => Some(*e),
            _ => None,
        };
        Response::ok(pb::AssertIdentityResponse {
            ticket: asserted.secret.expose().to_owned(),
            subject: subject.into(),
            sets: t.sets.iter().map(set_to_pb).collect(),
            expires_at: Some(wire::timestamp(t.expires_at)).into(),
            ..Default::default()
        })
    }
}
