// SPDX-License-Identifier: AGPL-3.0-or-later

//! `RepositoryService` (§5.3 of docs/06-storage.md).

use super::wire::{self, Refuse, name};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::backup::repository::CreateRepository;
use korpis_app::idempotency::Idempotent;
use korpis_core::backup::{Repository, TenantKey};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::repository::{custody_from_pb, repository_to_pb, retention_from_pb};

fn created(r: &Repository) -> pb::CreateRepositoryResponse {
    pb::CreateRepositoryResponse { repository: repository_to_pb(r).into(), ..Default::default() }
}

fn project(field: &str, p: Option<&str>) -> Result<Option<uuid::Uuid>, connectrpc::ConnectError> {
    match p {
        None | Some("") => Ok(None),
        Some(p) => Ok(Some(wire::id(IdKind::Project, field, p).refuse()?)),
    }
}

impl pb::RepositoryService for Api {
    async fn create_repository(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateRepositoryRequest>,
    ) -> ServiceResult<pb::CreateRepositoryResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let retention =
            req.retention.as_option().ok_or_else(|| error::invalid("retention", "required"))?;
        let cmd = CreateRepository {
            organization: wire::id(IdKind::Organization, "organization", &req.organization)
                .refuse()?,
            project: project("project", req.project.as_deref())?,
            name: name("name", &req.name).refuse()?,
            provider: name("provider", &req.provider).refuse()?,
            prefix: req.prefix.clone(),
            custody: custody_from_pb("key_custody", req.key_custody).refuse()?,
            retention: retention_from_pb("retention", retention).refuse()?,
            streams: req.streams,
            kms: match req.kms.as_option() {
                None => None,
                Some(k) => Some(TenantKey {
                    provider: name("kms.provider", &k.provider).refuse()?.as_str().to_owned(),
                    key: k.key.clone(),
                }),
            },
        };
        let encoder = |r: &Repository| encode(&created(r));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.create_repository(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, created)
    }

    async fn get_repository(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetRepositoryRequest>,
    ) -> ServiceResult<pb::GetRepositoryResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Repository, "id", request.id).refuse()?;
        let r = self.app.get_repository(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetRepositoryResponse {
            repository: repository_to_pb(&r).into(),
            ..Default::default()
        })
    }

    async fn list_repositories(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListRepositoriesRequest>,
    ) -> ServiceResult<pb::ListRepositoriesResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = self.page(req.page.as_option(), IdKind::Repository)?;
        let organization =
            wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let project = project("project", req.project.as_deref())?;
        let listed = self
            .app
            .list_repositories(&caller, organization, project, page)
            .await
            .map_err(error::from_app)?;
        Response::ok(pb::ListRepositoriesResponse {
            repositories: listed.items.iter().map(repository_to_pb).collect(),
            page: self.next_page(listed.next, IdKind::Repository)?.into(),
            ..Default::default()
        })
    }
}
