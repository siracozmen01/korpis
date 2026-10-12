// SPDX-License-Identifier: AGPL-3.0-or-later

//! `OrganizationService` and `ProjectService`.

use super::wire::tenancy::{labels, organization_to_pb, project_to_pb};
use super::wire::{self, name};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::tenancy::{CreateOrganization, CreateProject};
use korpis_app::{AppError, Idempotent};
use korpis_core::id::IdKind;
use korpis_core::tenancy::{Organization, Project};
use korpis_proto::korpis::v1 as pb;

fn created_organization(o: &Organization) -> pb::CreateOrganizationResponse {
    pb::CreateOrganizationResponse {
        organization: organization_to_pb(o).into(),
        ..Default::default()
    }
}

fn created_project(p: &Project) -> pb::CreateProjectResponse {
    pb::CreateProjectResponse { project: project_to_pb(p).into(), ..Default::default() }
}

/// Specified, scheduled, not yet built.
fn not_yet(what: &'static str, phase: u8) -> ConnectError {
    error::from_app(AppError::NotYet { what, phase })
}

impl pb::OrganizationService for Api {
    async fn create_organization(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateOrganizationRequest>,
    ) -> ServiceResult<pb::CreateOrganizationResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let parent = req.parent.as_deref().ok_or_else(|| error::invalid("parent", "required"))?;
        let cmd = CreateOrganization {
            name: name("name", &req.name)?,
            parent: wire::id(IdKind::Organization, "parent", parent)?,
        };
        let encoder = |o: &Organization| encode(&created_organization(o));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.create_organization(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, created_organization)
    }

    async fn get_organization(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetOrganizationRequest>,
    ) -> ServiceResult<pb::GetOrganizationResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Organization, "id", request.id)?;
        let o = self.app.get_organization(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetOrganizationResponse {
            organization: organization_to_pb(&o).into(),
            ..Default::default()
        })
    }

    async fn list_organizations(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListOrganizationsRequest>,
    ) -> ServiceResult<pb::ListOrganizationsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Organization)?;
        let parent = req
            .parent
            .as_deref()
            .map(|p| wire::id(IdKind::Organization, "parent", p))
            .transpose()?;
        let listed =
            self.app.list_organizations(&caller, parent, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListOrganizationsResponse {
            organizations: listed.items.iter().map(organization_to_pb).collect(),
            page: next_page(listed.next, IdKind::Organization).into(),
            ..Default::default()
        })
    }

    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn set_quota(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::SetQuotaRequest>,
    ) -> ServiceResult<pb::SetQuotaResponse> {
        Err(not_yet("quota sets", 2))
    }

    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn suspend_organization(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::SuspendOrganizationRequest>,
    ) -> ServiceResult<pb::SuspendOrganizationResponse> {
        Err(not_yet("organization suspension", 2))
    }

    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn resume_organization(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ResumeOrganizationRequest>,
    ) -> ServiceResult<pb::ResumeOrganizationResponse> {
        Err(not_yet("organization suspension", 2))
    }
}

impl pb::ProjectService for Api {
    async fn create_project(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateProjectRequest>,
    ) -> ServiceResult<pb::CreateProjectResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let cmd = CreateProject {
            organization: wire::id(IdKind::Organization, "organization", &req.organization)?,
            name: name("name", &req.name)?,
            labels: labels("labels", &req.labels)?,
        };
        let encoder = |p: &Project| encode(&created_project(p));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.create_project(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, created_project)
    }

    async fn get_project(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetProjectRequest>,
    ) -> ServiceResult<pb::GetProjectResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Project, "id", request.id)?;
        let p = self.app.get_project(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetProjectResponse {
            project: project_to_pb(&p).into(),
            ..Default::default()
        })
    }

    async fn list_projects(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListProjectsRequest>,
    ) -> ServiceResult<pb::ListProjectsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let org = wire::id(IdKind::Organization, "organization", &req.organization)?;
        let page = page(req.page.as_option(), IdKind::Project)?;
        let listed = self.app.list_projects(&caller, org, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListProjectsResponse {
            projects: listed.items.iter().map(project_to_pb).collect(),
            page: next_page(listed.next, IdKind::Project).into(),
            ..Default::default()
        })
    }
}
