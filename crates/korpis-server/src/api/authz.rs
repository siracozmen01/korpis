// SPDX-License-Identifier: AGPL-3.0-or-later

//! `GrantService` (§3 and §6 of docs/08-identity.md).

use super::wire::authz::{actions, conditions_from_pb, delegation_from_pb, grant_to_pb};
use super::wire::{self, principal, scope};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::authz::{GrantsFilter, IssueGrant, MintToken, Revoked};
use korpis_core::authz::GrantRecord;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::list_grants_request::Filter;
use std::time::{Duration, SystemTime};

fn issue_command(req: &pb::IssueGrantRequest) -> Result<IssueGrant, ConnectError> {
    let subject = req.subject.as_option().ok_or_else(|| error::invalid("subject", "required"))?;
    let scope_pb = req.scope.as_option().ok_or_else(|| error::invalid("scope", "required"))?;
    Ok(IssueGrant {
        parent: wire::id(IdKind::Grant, "parent", &req.parent)?,
        holder: principal::from_pb("subject", subject)?,
        actions: actions("actions", &req.actions)?,
        scope: scope::from_pb("scope", scope_pb)?,
        conditions: conditions_from_pb(req.conditions.as_option())?,
        delegation: delegation_from_pb(req.delegation.as_option())?,
        link_form: req.link_form,
    })
}

fn issued(g: &GrantRecord, now: SystemTime) -> pb::IssueGrantResponse {
    pb::IssueGrantResponse { grant: grant_to_pb(g, now).into(), ..Default::default() }
}

fn revoked(r: &Revoked, now: SystemTime) -> pb::RevokeGrantResponse {
    pb::RevokeGrantResponse {
        grant: grant_to_pb(&r.grant, now).into(),
        cascaded: r.cascaded,
        ..Default::default()
    }
}

fn filter(f: Option<&Filter>) -> Result<GrantsFilter, ConnectError> {
    Ok(match f {
        None => GrantsFilter::Mine,
        Some(Filter::Scope(s)) => GrantsFilter::On(scope::object_from_pb("scope", s)?),
        Some(Filter::Subject(s)) => GrantsFilter::HeldBy(principal::from_pb("subject", s)?),
    })
}

impl pb::GrantService for Api {
    async fn issue_grant(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::IssueGrantRequest>,
    ) -> ServiceResult<pb::IssueGrantResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let cmd = issue_command(&req)?;
        let now = self.app.now();
        let encoder = |g: &GrantRecord| encode(&issued(g, now));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.issue_grant(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, |g| issued(g, now))
    }

    async fn get_grant(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetGrantRequest>,
    ) -> ServiceResult<pb::GetGrantResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Grant, "id", request.id)?;
        let found = self.app.get_grant(&caller, id).await.map_err(error::from_app)?;
        let now = self.app.now();
        Response::ok(pb::GetGrantResponse {
            grant: grant_to_pb(&found.grant, now).into(),
            chain: found.chain.iter().map(|g| grant_to_pb(g, now)).collect(),
            ..Default::default()
        })
    }

    async fn list_grants(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListGrantsRequest>,
    ) -> ServiceResult<pb::ListGrantsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Grant)?;
        let filter = filter(req.filter.as_ref())?;
        let listed = self.app.list_grants(&caller, filter, page).await.map_err(error::from_app)?;
        let now = self.app.now();
        Response::ok(pb::ListGrantsResponse {
            grants: listed.items.iter().map(|g| grant_to_pb(g, now)).collect(),
            page: next_page(listed.next, IdKind::Grant).into(),
            ..Default::default()
        })
    }

    async fn revoke_grant(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RevokeGrantRequest>,
    ) -> ServiceResult<pb::RevokeGrantResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let grant = wire::id(IdKind::Grant, "grant", &req.grant)?;
        let now = self.app.now();
        let encoder = |r: &Revoked| encode(&revoked(r, now));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.revoke_grant(&caller, &idem, grant).await.map_err(error::from_app)?;
        respond(reply, |r| revoked(r, now))
    }

    async fn mint_token(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::MintTokenRequest>,
    ) -> ServiceResult<pb::MintTokenResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let scope_pb = req.scope.as_option().ok_or_else(|| error::invalid("scope", "required"))?;
        let cmd = MintToken {
            via: (!req.grant.is_empty())
                .then(|| wire::id(IdKind::Grant, "grant", &req.grant))
                .transpose()?,
            actions: req.actions.clone(),
            object: scope::object_from_pb("scope", scope_pb)?,
            ttl: (req.ttl_seconds > 0).then(|| Duration::from_secs(u64::from(req.ttl_seconds))),
        };
        let minted = self.app.mint_token(&caller, cmd).await.map_err(error::from_app)?;
        Response::ok(pb::MintTokenResponse {
            token: minted.token,
            expires_at: wire::timestamp(minted.expires_at).into(),
            ..Default::default()
        })
    }
}
