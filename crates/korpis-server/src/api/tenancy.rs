// SPDX-License-Identifier: AGPL-3.0-or-later

//! `OrganizationService` and `ProjectService`.

use super::wire::Refuse;
use super::wire::tenancy::{labels, organization_to_pb, project_to_pb};
use super::wire::{self, name};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::tenancy::{CreateOrganization, CreateProject};
use korpis_core::id::IdKind;
use korpis_core::tenancy::{Organization, Project};
use korpis_proto::korpis::v1 as pb;
use korpis_wire::quota::quota_set_to_pb;

fn created_organization(o: &Organization) -> pb::CreateOrganizationResponse {
    pb::CreateOrganizationResponse {
        organization: organization_to_pb(o).into(),
        ..Default::default()
    }
}

fn created_project(p: &Project) -> pb::CreateProjectResponse {
    pb::CreateProjectResponse { project: project_to_pb(p).into(), ..Default::default() }
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
            name: name("name", &req.name).refuse()?,
            parent: wire::id(IdKind::Organization, "parent", parent).refuse()?,
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
        let id = wire::id(IdKind::Organization, "id", request.id).refuse()?;
        let o = self.app.get_organization(&caller, id).await.map_err(error::from_app)?;
        let owner = korpis_core::tenancy::quota::QuotaOwner::Organization(id);
        let quota = self.app.quota_set(owner).await.map_err(error::from_app)?;
        let organization =
            pb::Organization { quota: quota_set_to_pb(&quota).into(), ..organization_to_pb(&o) };
        Response::ok(pb::GetOrganizationResponse {
            organization: organization.into(),
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
        let page = self.page(req.page.as_option(), IdKind::Organization)?;
        let parent = req
            .parent
            .as_deref()
            .map(|p| wire::id(IdKind::Organization, "parent", p))
            .transpose()
            .refuse()?;
        let listed =
            self.app.list_organizations(&caller, parent, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListOrganizationsResponse {
            organizations: listed.items.iter().map(organization_to_pb).collect(),
            page: self.next_page(listed.next, IdKind::Organization)?.into(),
            ..Default::default()
        })
    }

    async fn set_quota(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetQuotaRequest>,
    ) -> ServiceResult<pb::SetQuotaResponse> {
        super::quota::set_quota(self, &ctx, request).await
    }

    async fn suspend_organization(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SuspendOrganizationRequest>,
    ) -> ServiceResult<pb::SuspendOrganizationResponse> {
        super::suspension::suspend(self, &ctx, request).await
    }

    async fn resume_organization(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ResumeOrganizationRequest>,
    ) -> ServiceResult<pb::ResumeOrganizationResponse> {
        super::suspension::resume(self, &ctx, request).await
    }

    async fn get_trust_policy(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetTrustPolicyRequest>,
    ) -> ServiceResult<pb::GetTrustPolicyResponse> {
        super::trust::get(self, &ctx, request).await
    }

    async fn set_trust_policy(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetTrustPolicyRequest>,
    ) -> ServiceResult<pb::SetTrustPolicyResponse> {
        super::trust::set(self, &ctx, request).await
    }

    async fn get_delegation_ceiling(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetDelegationCeilingRequest>,
    ) -> ServiceResult<pb::GetDelegationCeilingResponse> {
        super::ceiling::get(self, &ctx, request).await
    }

    async fn set_delegation_ceiling(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetDelegationCeilingRequest>,
    ) -> ServiceResult<pb::SetDelegationCeilingResponse> {
        super::ceiling::set(self, &ctx, request).await
    }

    async fn get_theme(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetThemeRequest>,
    ) -> ServiceResult<pb::GetThemeResponse> {
        super::theme::get(self, &ctx, request).await
    }

    async fn set_theme(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetThemeRequest>,
    ) -> ServiceResult<pb::SetThemeResponse> {
        super::theme::set(self, &ctx, request).await
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
            organization: wire::id(IdKind::Organization, "organization", &req.organization)
                .refuse()?,
            name: name("name", &req.name).refuse()?,
            labels: labels("labels", &req.labels).refuse()?,
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
        let id = wire::id(IdKind::Project, "id", request.id).refuse()?;
        let p = self.app.get_project(&caller, id).await.map_err(error::from_app)?;
        let owner = korpis_core::tenancy::quota::QuotaOwner::Project(id);
        let quota = self.app.quota_set(owner).await.map_err(error::from_app)?;
        let project = pb::Project { quota: quota_set_to_pb(&quota).into(), ..project_to_pb(&p) };
        Response::ok(pb::GetProjectResponse { project: project.into(), ..Default::default() })
    }

    async fn list_projects(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListProjectsRequest>,
    ) -> ServiceResult<pb::ListProjectsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let org = wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let page = self.page(req.page.as_option(), IdKind::Project)?;
        let listed = self.app.list_projects(&caller, org, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListProjectsResponse {
            projects: listed.items.iter().map(project_to_pb).collect(),
            page: self.next_page(listed.next, IdKind::Project)?.into(),
            ..Default::default()
        })
    }
}
