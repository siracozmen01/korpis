// SPDX-License-Identifier: AGPL-3.0-or-later

//! `PortalService` (§3.6 of docs/07-networking.md).

use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::idempotency::Idempotent;
use korpis_app::portal::{CreatePortal, PortalView};
use korpis_core::dns::domain::DomainName;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::portal::portal_to_pb;
use uuid::Uuid;

fn shown(v: &PortalView) -> pb::Portal {
    portal_to_pb(&v.portal, v.certificate.as_ref())
}

fn created(v: &PortalView) -> pb::CreatePortalResponse {
    pb::CreatePortalResponse { portal: shown(v).into(), ..Default::default() }
}

fn changed(v: &PortalView) -> pb::SetPortalCertificateResponse {
    pb::SetPortalCertificateResponse { portal: shown(v).into(), ..Default::default() }
}

fn deleted() -> pb::DeletePortalResponse {
    pb::DeletePortalResponse::default()
}

/// The provided certificate a request names in `field`, or none for the portal's own.
fn certificate(field: &str, value: &str) -> Result<Option<Uuid>, connectrpc::ConnectError> {
    if value.is_empty() {
        return Ok(None);
    }
    Ok(Some(wire::id(IdKind::Certificate, field, value).refuse()?))
}

impl pb::PortalService for Api {
    async fn create_portal(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreatePortalRequest>,
    ) -> ServiceResult<pb::CreatePortalResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let name = DomainName::parse(&req.name).ok_or_else(|| {
            error::invalid("name", "a lowercase DNS name of two labels or more, without a wildcard")
        })?;
        let cmd = CreatePortal {
            organization: wire::id(IdKind::Organization, "organization", &req.organization)
                .refuse()?,
            name,
            certificate: certificate("certificate", &req.certificate)?,
        };
        let encoder = |v: &PortalView| encode(&created(v));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.create_portal(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, created)
    }

    async fn get_portal(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetPortalRequest>,
    ) -> ServiceResult<pb::GetPortalResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Portal, "id", request.id).refuse()?;
        let v = self.app.get_portal(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetPortalResponse { portal: shown(&v).into(), ..Default::default() })
    }

    async fn list_portals(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListPortalsRequest>,
    ) -> ServiceResult<pb::ListPortalsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = self.page(req.page.as_option(), IdKind::Portal)?;
        let organization =
            wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let listed =
            self.app.list_portals(&caller, organization, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListPortalsResponse {
            portals: listed.items.iter().map(shown).collect(),
            page: self.next_page(listed.next, IdKind::Portal)?.into(),
            ..Default::default()
        })
    }

    async fn set_portal_certificate(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetPortalCertificateRequest>,
    ) -> ServiceResult<pb::SetPortalCertificateResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Portal, "id", &req.id).refuse()?;
        let named = certificate("certificate", &req.certificate)?;
        let encoder = |v: &PortalView| encode(&changed(v));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .set_portal_certificate(&caller, &idem, (id, named))
            .await
            .map_err(error::from_app)?;
        respond(reply, changed)
    }

    async fn delete_portal(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeletePortalRequest>,
    ) -> ServiceResult<pb::DeletePortalResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Portal, "id", &req.id).refuse()?;
        let encoder = |(): &()| encode(&deleted());
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.delete_portal(&caller, &idem, id).await.map_err(error::from_app)?;
        respond(reply, |(): &()| deleted())
    }
}
