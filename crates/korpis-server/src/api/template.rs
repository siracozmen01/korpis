// SPDX-License-Identifier: AGPL-3.0-or-later

//! The grant template calls of `GrantService` (§4 of docs/08-identity.md).

use super::wire::Refuse;
use super::wire::scope::object_ref_from_pb;
use super::wire::template::{body_from_pb, template_to_pb};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::authz::{TemplateAsked, TemplatesOf};
use korpis_core::authz::template::GrantTemplate;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::list_grant_templates_request::Of;

fn asked(b: Option<&pb::GrantTemplateBody>) -> Result<TemplateAsked, ConnectError> {
    let b = b.ok_or_else(|| error::invalid("template", "required"))?;
    let (name, body) = body_from_pb(b).refuse()?;
    Ok(TemplateAsked { name, body })
}

fn id(field: &str, value: &str) -> Result<uuid::Uuid, ConnectError> {
    super::wire::id(IdKind::Template, field, value).refuse()
}

impl Api {
    pub(super) async fn create_template(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateGrantTemplateRequest>,
    ) -> ServiceResult<pb::CreateGrantTemplateResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let owner =
            super::wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let asked = asked(req.template.as_option())?;
        let made = |t: &GrantTemplate| pb::CreateGrantTemplateResponse {
            template: template_to_pb(t).into(),
            ..Default::default()
        };
        let encoder = |t: &GrantTemplate| encode(&made(t));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .create_grant_template(&caller, &idem, owner, asked)
            .await
            .map_err(error::from_app)?;
        respond(reply, made)
    }

    pub(super) async fn update_template(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::UpdateGrantTemplateRequest>,
    ) -> ServiceResult<pb::UpdateGrantTemplateResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let expected = req
            .mutation
            .as_option()
            .and_then(|m| m.expected_version)
            .ok_or_else(|| error::invalid("mutation.expected_version", "the version edited"))?;
        let template = id("id", &req.id)?;
        let asked = asked(req.template.as_option())?;
        let made = |t: &GrantTemplate| pb::UpdateGrantTemplateResponse {
            template: template_to_pb(t).into(),
            ..Default::default()
        };
        let encoder = |t: &GrantTemplate| encode(&made(t));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .update_grant_template(&caller, &idem, template, expected, asked)
            .await
            .map_err(error::from_app)?;
        respond(reply, made)
    }

    pub(super) async fn retire_template(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RetireGrantTemplateRequest>,
    ) -> ServiceResult<pb::RetireGrantTemplateResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let template = id("id", &req.id)?;
        let made = |t: &GrantTemplate| pb::RetireGrantTemplateResponse {
            template: template_to_pb(t).into(),
            ..Default::default()
        };
        let encoder = |t: &GrantTemplate| encode(&made(t));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self
            .app
            .retire_grant_template(&caller, &idem, template)
            .await
            .map_err(error::from_app)?;
        respond(reply, made)
    }

    pub(super) async fn get_template(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetGrantTemplateRequest>,
    ) -> ServiceResult<pb::GetGrantTemplateResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let template = id("id", &req.id)?;
        let at = req.at.as_option().map(|a| object_ref_from_pb("at", a)).transpose().refuse()?;
        let found = self
            .app
            .get_grant_template(&caller, template, req.version, at)
            .await
            .map_err(error::from_app)?;
        Response::ok(pb::GetGrantTemplateResponse {
            template: template_to_pb(&found).into(),
            ..Default::default()
        })
    }

    pub(super) async fn list_templates(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListGrantTemplatesRequest>,
    ) -> ServiceResult<pb::ListGrantTemplatesResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Template)?;
        let of = match req.of.as_ref() {
            Some(Of::At(at)) => TemplatesOf::At(object_ref_from_pb("at", at).refuse()?),
            Some(Of::Owned(org)) => {
                TemplatesOf::Owned(super::wire::id(IdKind::Organization, "owned", org).refuse()?)
            }
            None => return Err(error::invalid("of", "an object, or an organization's own")),
        };
        let listed =
            self.app.list_grant_templates(&caller, of, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListGrantTemplatesResponse {
            templates: listed.items.iter().map(template_to_pb).collect(),
            page: next_page(listed.next, IdKind::Template).into(),
            ..Default::default()
        })
    }
}
