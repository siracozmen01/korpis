// SPDX-License-Identifier: AGPL-3.0-or-later

//! `IdentityService` (§5 of docs/08-identity.md): identity bindings.

use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::authz::BindingAsked;
use korpis_app::idempotency::Idempotent;
use korpis_core::authz::binding::{IdentityBinding, ProviderRef};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::binding::{binding_to_pb, subject_from_pb, terms_from_pb};

fn id(value: &str) -> Result<uuid::Uuid, ConnectError> {
    wire::id(IdKind::Binding, "id", value).refuse()
}

impl pb::IdentityService for Api {
    async fn create_identity_binding(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateIdentityBindingRequest>,
    ) -> ServiceResult<pb::CreateIdentityBindingResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let organization =
            wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let asked = BindingAsked {
            provider: ProviderRef { organization, name: req.provider.clone() },
            subject: subject_from_pb("subject", req.subject.as_option()).refuse()?,
            terms: terms_from_pb(req.terms.as_option()).refuse()?,
        };
        let made = |b: &IdentityBinding| pb::CreateIdentityBindingResponse {
            binding: binding_to_pb(b).into(),
            ..Default::default()
        };
        let encoder = |b: &IdentityBinding| encode(&made(b));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .create_identity_binding(&caller, &idem, asked)
            .await
            .map_err(error::from_app)?;
        respond(reply, made)
    }

    async fn update_identity_binding(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::UpdateIdentityBindingRequest>,
    ) -> ServiceResult<pb::UpdateIdentityBindingResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let expected = req
            .mutation
            .as_option()
            .and_then(|m| m.expected_version)
            .ok_or_else(|| error::invalid("mutation.expected_version", "the version edited"))?;
        let binding = id(&req.id)?;
        let terms = terms_from_pb(req.terms.as_option()).refuse()?;
        let made = |b: &IdentityBinding| pb::UpdateIdentityBindingResponse {
            binding: binding_to_pb(b).into(),
            ..Default::default()
        };
        let encoder = |b: &IdentityBinding| encode(&made(b));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .update_identity_binding(&caller, &idem, binding, expected, terms)
            .await
            .map_err(error::from_app)?;
        respond(reply, made)
    }

    async fn retire_identity_binding(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RetireIdentityBindingRequest>,
    ) -> ServiceResult<pb::RetireIdentityBindingResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let binding = id(&req.id)?;
        let made = |b: &IdentityBinding| pb::RetireIdentityBindingResponse {
            binding: binding_to_pb(b).into(),
            ..Default::default()
        };
        let encoder = |b: &IdentityBinding| encode(&made(b));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .retire_identity_binding(&caller, &idem, binding)
            .await
            .map_err(error::from_app)?;
        respond(reply, made)
    }

    async fn get_identity_binding(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetIdentityBindingRequest>,
    ) -> ServiceResult<pb::GetIdentityBindingResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let found =
            self.app.get_identity_binding(&caller, id(&req.id)?).await.map_err(error::from_app)?;
        Response::ok(pb::GetIdentityBindingResponse {
            binding: binding_to_pb(&found).into(),
            ..Default::default()
        })
    }

    async fn list_identity_bindings(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListIdentityBindingsRequest>,
    ) -> ServiceResult<pb::ListIdentityBindingsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let org = wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let page = self.page(req.page.as_option(), IdKind::Binding)?;
        let listed = self
            .app
            .list_identity_bindings(&caller, org, req.retired, page)
            .await
            .map_err(error::from_app)?;
        Response::ok(pb::ListIdentityBindingsResponse {
            bindings: listed.items.iter().map(binding_to_pb).collect(),
            page: self.next_page(listed.next, IdKind::Binding)?.into(),
            ..Default::default()
        })
    }
}
