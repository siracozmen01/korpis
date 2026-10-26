// SPDX-License-Identifier: AGPL-3.0-or-later

//! `AddressPoolService` (§3.7 of docs/07-networking.md).

use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::address::{AddressPoolView, CreateAddressPool};
use korpis_app::idempotency::Idempotent;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::address::{pool_to_pb, routing_from_pb, scope_from_pb};

fn shown(v: &AddressPoolView) -> pb::AddressPool {
    pool_to_pb(&v.pool, v.held)
}

fn created(v: &AddressPoolView) -> pb::CreateAddressPoolResponse {
    pb::CreateAddressPoolResponse { pool: shown(v).into(), ..Default::default() }
}

fn deleted() -> pb::DeleteAddressPoolResponse {
    pb::DeleteAddressPoolResponse::default()
}

/// The pool a request asks for.
fn asked(p: &pb::AddressPool) -> Result<CreateAddressPool, connectrpc::ConnectError> {
    let cidr = p.cidr.parse().map_err(|_| error::invalid("pool.cidr", "a network, as a CIDR"))?;
    let reserved = p.reserved.iter().map(|a| a.parse()).collect::<Result<Vec<_>, _>>();
    let quarantine = match p.quarantine.as_option() {
        Some(d) => Some(korpis_wire::duration_from("pool.quarantine", d).refuse()?),
        None => None,
    };
    Ok(CreateAddressPool {
        organization: wire::id(IdKind::Organization, "pool.organization", &p.organization)
            .refuse()?,
        cidr,
        scope: scope_from_pb(p).refuse()?,
        routing: routing_from_pb(p).refuse()?,
        interface: p.interface.clone(),
        quarantine,
        reserved: reserved.map_err(|_| error::invalid("pool.reserved", "addresses"))?,
    })
}

impl pb::AddressPoolService for Api {
    async fn create_address_pool(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateAddressPoolRequest>,
    ) -> ServiceResult<pb::CreateAddressPoolResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let pool = req.pool.as_option().ok_or_else(|| error::invalid("pool", "required"))?;
        let cmd = asked(pool)?;
        let encoder = |v: &AddressPoolView| encode(&created(v));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.create_address_pool(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, created)
    }

    async fn list_address_pools(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListAddressPoolsRequest>,
    ) -> ServiceResult<pb::ListAddressPoolsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = self.page(req.page.as_option(), IdKind::AddressPool)?;
        let organization =
            wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let listed = self
            .app
            .list_address_pools(&caller, organization, page)
            .await
            .map_err(error::from_app)?;
        Response::ok(pb::ListAddressPoolsResponse {
            pools: listed.items.iter().map(shown).collect(),
            page: self.next_page(listed.next, IdKind::AddressPool)?.into(),
            ..Default::default()
        })
    }

    async fn delete_address_pool(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeleteAddressPoolRequest>,
    ) -> ServiceResult<pb::DeleteAddressPoolResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::AddressPool, "id", &req.id).refuse()?;
        let encoder = |(): &()| encode(&deleted());
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.delete_address_pool(&caller, &idem, id).await.map_err(error::from_app)?;
        respond(reply, |(): &()| deleted())
    }
}
