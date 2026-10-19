// SPDX-License-Identifier: AGPL-3.0-or-later

//! `GrantService` (§3 and §6 of docs/08-identity.md).

use super::wire::Refuse;
use super::wire::authz::{actions, conditions_from_pb, delegation_from_pb, grant_to_pb};
use super::wire::{self, principal, scope};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::authz::{
    FromTemplate, GrantsFilter, Holder, IssueGrant, Issued, MintToken, Revoked,
};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::list_grants_request::Filter;
use std::time::{Duration, SystemTime};

fn holder(req: &pb::IssueGrantRequest) -> Result<Holder, ConnectError> {
    if let Some(user) = req.to_new_user.as_option() {
        if req.subject.as_option().is_some() || req.to_new_token || req.link_form {
            return Err(error::invalid(
                "to_new_user",
                "alone: not with subject, to_new_token, or link_form",
            ));
        }
        let invitation_expires_at = user
            .invitation_expires_at
            .as_option()
            .map(|t| wire::time("to_new_user.invitation_expires_at", t))
            .transpose()
            .refuse()?;
        return Ok(Holder::NewUser {
            display_name: user.display_name.clone(),
            invitation_expires_at,
        });
    }
    match (req.subject.as_option(), req.to_new_token, req.link_form) {
        (Some(s), false, false) => {
            Ok(Holder::Principal(principal::from_pb("subject", s).refuse()?))
        }
        (None, true, false) => Ok(Holder::NewToken),
        (None, false, true) => Ok(Holder::Link),
        (None, true, true) => Err(error::invalid("link_form", "a link is not held by a token")),
        (Some(_), ..) => {
            Err(error::invalid("subject", "absent when to_new_token or link_form is set"))
        }
        (None, false, false) => Err(error::invalid("subject", "required")),
    }
}

fn issue_command(req: &pb::IssueGrantRequest) -> Result<IssueGrant, ConnectError> {
    let scope_pb = req.scope.as_option().ok_or_else(|| error::invalid("scope", "required"))?;
    Ok(IssueGrant {
        parent: (!req.parent.is_empty())
            .then(|| wire::id(IdKind::Grant, "parent", &req.parent))
            .transpose()
            .refuse()?,
        holder: holder(req)?,
        actions: actions("actions", &req.actions).refuse()?,
        scope: scope::from_pb("scope", scope_pb).refuse()?,
        conditions: conditions_from_pb(req.conditions.as_option()).refuse()?,
        delegation: delegation_from_pb(req.delegation.as_option()).refuse()?,
        label: req.label.clone(),
        template: req
            .template
            .as_option()
            .map(|t| {
                Ok::<_, ConnectError>(FromTemplate {
                    template: wire::id(IdKind::Template, "template.template", &t.template)
                        .refuse()?,
                    version: t.version,
                    conditions: req
                        .conditions
                        .as_option()
                        .map(|c| conditions_from_pb(Some(c)))
                        .transpose()
                        .refuse()?,
                })
            })
            .transpose()?,
    })
}

/// The answer to an issue; the record kept for a replay leaves the token's secret, the link, and
/// the invitation out, because a secret recorded is a secret stored (§6 of docs/10-api.md).
fn issued(i: &Issued, now: SystemTime, secret: bool) -> pb::IssueGrantResponse {
    let token = i.secret.as_ref().filter(|_| secret).map(|s| s.expose().to_owned());
    let link = i.link.clone().filter(|_| secret);
    let invitation = i.invitation.as_ref().filter(|_| secret).map(|s| s.expose().to_owned());
    pb::IssueGrantResponse {
        grant: grant_to_pb(&i.record, now).into(),
        token: token.unwrap_or_default(),
        link_token: link.unwrap_or_default(),
        invitation: invitation.unwrap_or_default(),
        ..Default::default()
    }
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
        Some(Filter::Scope(s)) => GrantsFilter::On(scope::object_from_pb("scope", s).refuse()?),
        Some(Filter::Subject(s)) => {
            GrantsFilter::HeldBy(principal::from_pb("subject", s).refuse()?)
        }
        Some(Filter::Template(t)) => {
            GrantsFilter::FromTemplate(wire::id(IdKind::Template, "template", t).refuse()?)
        }
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
        let encoder = |i: &Issued| encode(&issued(i, now, false));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.issue_grant(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, |i| issued(i, now, true))
    }

    async fn get_grant(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetGrantRequest>,
    ) -> ServiceResult<pb::GetGrantResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Grant, "id", request.id).refuse()?;
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
        let grant = wire::id(IdKind::Grant, "grant", &req.grant).refuse()?;
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
                .transpose()
                .refuse()?,
            actions: req.actions.clone(),
            object: scope::object_from_pb("scope", scope_pb).refuse()?,
            ttl: (req.ttl_seconds > 0).then(|| Duration::from_secs(u64::from(req.ttl_seconds))),
            renews: (!req.renews.is_empty()).then(|| req.renews.clone()),
        };
        let minted = self.app.mint_token(&caller, cmd).await.map_err(error::from_app)?;
        Response::ok(pb::MintTokenResponse {
            token: minted.token,
            expires_at: wire::timestamp(minted.expires_at).into(),
            route: minted.route.unwrap_or_default(),
            ..Default::default()
        })
    }

    async fn create_grant_template(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateGrantTemplateRequest>,
    ) -> ServiceResult<pb::CreateGrantTemplateResponse> {
        self.create_template(ctx, request).await
    }

    async fn update_grant_template(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::UpdateGrantTemplateRequest>,
    ) -> ServiceResult<pb::UpdateGrantTemplateResponse> {
        self.update_template(ctx, request).await
    }

    async fn retire_grant_template(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RetireGrantTemplateRequest>,
    ) -> ServiceResult<pb::RetireGrantTemplateResponse> {
        self.retire_template(ctx, request).await
    }

    async fn get_grant_template(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetGrantTemplateRequest>,
    ) -> ServiceResult<pb::GetGrantTemplateResponse> {
        self.get_template(ctx, request).await
    }

    async fn list_grant_templates(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListGrantTemplatesRequest>,
    ) -> ServiceResult<pb::ListGrantTemplatesResponse> {
        self.list_templates(ctx, request).await
    }
}
