// SPDX-License-Identifier: AGPL-3.0-or-later

//! `DomainService` (§3.2 of docs/07-networking.md).

use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::domain::{CreateDomain, Requested};
use korpis_app::idempotency::Idempotent;
use korpis_core::dns::domain::{Domain, DomainName};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::domain::domain_to_pb;

fn created(d: &Domain) -> pb::CreateDomainResponse {
    pb::CreateDomainResponse { domain: domain_to_pb(d).into(), ..Default::default() }
}

fn verified(d: &Domain) -> pb::VerifyDomainResponse {
    pb::VerifyDomainResponse { domain: domain_to_pb(d).into(), ..Default::default() }
}

fn deleted(n: u64) -> pb::DeleteDomainResponse {
    pb::DeleteDomainResponse { deleted: n, ..Default::default() }
}

/// How the request asks for its domain to be verified.
fn requested(req: &pb::CreateDomainRequest) -> Result<Requested, connectrpc::ConnectError> {
    use pb::DomainVerification as V;
    let parent = req.parent.as_deref().filter(|p| !p.is_empty());
    match (req.verification.as_known(), parent) {
        (Some(V::DOMAIN_VERIFICATION_DNS_TXT | V::DOMAIN_VERIFICATION_UNSPECIFIED), None) => {
            Ok(Requested::DnsTxt)
        }
        (Some(V::DOMAIN_VERIFICATION_DELEGATED), Some(p)) => {
            Ok(Requested::Delegated { parent: wire::id(IdKind::Domain, "parent", p).refuse()? })
        }
        (Some(V::DOMAIN_VERIFICATION_OPERATOR), None) => Ok(Requested::Operator),
        (Some(V::DOMAIN_VERIFICATION_DELEGATED), None) => {
            Err(error::invalid("parent", "a delegated domain names the domain it lies beneath"))
        }
        _ => Err(error::invalid("parent", "only a delegated domain names a parent")),
    }
}

impl pb::DomainService for Api {
    async fn create_domain(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateDomainRequest>,
    ) -> ServiceResult<pb::CreateDomainResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let name = DomainName::parse(&req.name).ok_or_else(|| {
            error::invalid("name", "a lowercase DNS name of two labels or more, without a wildcard")
        })?;
        let cmd = CreateDomain {
            organization: wire::id(IdKind::Organization, "organization", &req.organization)
                .refuse()?,
            name,
            how: requested(&req)?,
        };
        let encoder = |d: &Domain| encode(&created(d));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.create_domain(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, created)
    }

    async fn verify_domain(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::VerifyDomainRequest>,
    ) -> ServiceResult<pb::VerifyDomainResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Domain, "id", &req.id).refuse()?;
        let encoder = |d: &Domain| encode(&verified(d));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.verify_domain(&caller, &idem, id).await.map_err(error::from_app)?;
        respond(reply, verified)
    }

    async fn get_domain(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetDomainRequest>,
    ) -> ServiceResult<pb::GetDomainResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Domain, "id", request.id).refuse()?;
        let d = self.app.get_domain(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetDomainResponse {
            domain: domain_to_pb(&d).into(),
            ..Default::default()
        })
    }

    async fn list_domains(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListDomainsRequest>,
    ) -> ServiceResult<pb::ListDomainsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = self.page(req.page.as_option(), IdKind::Domain)?;
        let organization =
            wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let listed =
            self.app.list_domains(&caller, organization, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListDomainsResponse {
            domains: listed.items.iter().map(domain_to_pb).collect(),
            page: self.next_page(listed.next, IdKind::Domain)?.into(),
            ..Default::default()
        })
    }

    async fn delete_domain(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeleteDomainRequest>,
    ) -> ServiceResult<pb::DeleteDomainResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Domain, "id", &req.id).refuse()?;
        let encoder = |n: &u64| encode(&deleted(*n));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.delete_domain(&caller, &idem, id).await.map_err(error::from_app)?;
        respond(reply, |n: &u64| deleted(*n))
    }
}
