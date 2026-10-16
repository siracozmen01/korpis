// SPDX-License-Identifier: AGPL-3.0-or-later

//! `CertificateService` (§3.2 of docs/07-networking.md). A key arrives in an upload and is
//! answered never.

use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::certificate::Upload;
use korpis_app::idempotency::Idempotent;
use korpis_core::dns::certificate::Certificate;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::certificate::certificate_to_pb;
use zeroize::Zeroizing;

fn created(c: &Certificate) -> pb::CreateCertificateResponse {
    pb::CreateCertificateResponse { certificate: certificate_to_pb(c).into(), ..Default::default() }
}

fn replaced(c: &Certificate) -> pb::ReplaceCertificateResponse {
    pb::ReplaceCertificateResponse {
        certificate: certificate_to_pb(c).into(),
        ..Default::default()
    }
}

fn deleted() -> pb::DeleteCertificateResponse {
    pb::DeleteCertificateResponse::default()
}

impl pb::CertificateService for Api {
    async fn create_certificate(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateCertificateRequest>,
    ) -> ServiceResult<pb::CreateCertificateResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let organization =
            wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let upload = Upload { chain: req.chain, key: Zeroizing::new(req.key) };
        let encoder = |c: &Certificate| encode(&created(c));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .create_certificate(&caller, &idem, (organization, upload))
            .await
            .map_err(error::from_app)?;
        respond(reply, created)
    }

    async fn replace_certificate(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ReplaceCertificateRequest>,
    ) -> ServiceResult<pb::ReplaceCertificateResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Certificate, "id", &req.id).refuse()?;
        let upload = Upload { chain: req.chain, key: Zeroizing::new(req.key) };
        let encoder = |c: &Certificate| encode(&replaced(c));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .replace_certificate(&caller, &idem, (id, upload))
            .await
            .map_err(error::from_app)?;
        respond(reply, replaced)
    }

    async fn get_certificate(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetCertificateRequest>,
    ) -> ServiceResult<pb::GetCertificateResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Certificate, "id", request.id).refuse()?;
        let c = self.app.get_certificate(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetCertificateResponse {
            certificate: certificate_to_pb(&c).into(),
            ..Default::default()
        })
    }

    async fn list_certificates(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListCertificatesRequest>,
    ) -> ServiceResult<pb::ListCertificatesResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Certificate)?;
        let organization =
            wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let listed = self
            .app
            .list_certificates(&caller, organization, page)
            .await
            .map_err(error::from_app)?;
        Response::ok(pb::ListCertificatesResponse {
            certificates: listed.items.iter().map(certificate_to_pb).collect(),
            page: next_page(listed.next, IdKind::Certificate).into(),
            ..Default::default()
        })
    }

    async fn delete_certificate(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeleteCertificateRequest>,
    ) -> ServiceResult<pb::DeleteCertificateResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Certificate, "id", &req.id).refuse()?;
        let encoder = |(): &()| encode(&deleted());
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.delete_certificate(&caller, &idem, id).await.map_err(error::from_app)?;
        respond(reply, |(): &()| deleted())
    }
}
