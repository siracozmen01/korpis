// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ExtensionService` (§4 of docs/16-extensions.md): installing an extension and reading what is
//! installed.

use super::wire::event::filter_to_pb;
use super::wire::extension::{manifest_to_pb, need_from_pb, provided_in_pb};
use super::wire::plan::plan_to_pb;
use super::wire::recipe::ref_to_pb;
use super::wire::value::map_from_pb;
use super::wire::{self, Refuse, principal, timestamp, wire_id};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::extension::{
    ExtensionStatus, ExtensionView, InstallExtension, Installed, Uninstalled, UpgradeExtension,
    Upgraded,
};
use korpis_core::extension::{ExtensionManifest, Installation, Need};
use korpis_core::id::IdKind;
use korpis_core::recipe::RecipeRef;
use korpis_proto::korpis::v1 as pb;
use uuid::Uuid;

fn status_to_pb(s: ExtensionStatus) -> pb::ExtensionStatus {
    match s {
        ExtensionStatus::Running => pb::ExtensionStatus::EXTENSION_STATUS_RUNNING,
        ExtensionStatus::Stopped => pb::ExtensionStatus::EXTENSION_STATUS_STOPPED,
        ExtensionStatus::Failed => pb::ExtensionStatus::EXTENSION_STATUS_FAILED,
    }
}

/// An installation as the wire shows it.
fn extension_to_pb(
    i: &Installation,
    manifest: &ExtensionManifest,
    artifact: &RecipeRef,
    grants: impl Iterator<Item = Uuid>,
    status: ExtensionStatus,
) -> pb::Extension {
    pb::Extension {
        id: wire_id(IdKind::Extension, i.id),
        name: i.name.clone(),
        version: i.version.clone(),
        api_version: i.api_version.clone(),
        ui_slots: manifest.ui_slots.iter().map(|s| s.slot.clone()).collect(),
        status: status_to_pb(status).into(),
        grants: grants.map(|g| wire_id(IdKind::Grant, g)).collect(),
        organization: wire_id(IdKind::Organization, i.organization),
        project: i.project.map(|p| wire_id(IdKind::Project, p)),
        workload: wire_id(IdKind::Workload, i.workload),
        artifact: ref_to_pb(artifact).into(),
        manifest: manifest_to_pb(manifest).into(),
        provides: manifest.provides.iter().map(|p| provided_in_pb(manifest, p)).collect(),
        subscribes: manifest.subscribes.iter().map(filter_to_pb).collect(),
        installed_by: principal::to_pb(&i.installed_by).into(),
        installed_at: timestamp(i.installed_at).into(),
        ..Default::default()
    }
}

fn view_to_pb(v: &ExtensionView) -> pb::Extension {
    let grants = v.grants.iter().copied();
    extension_to_pb(&v.installation, &v.manifest, &v.artifact, grants, v.status)
}

fn installed(i: &Installed) -> pb::InstallExtensionResponse {
    let grants = i.grants.iter().map(|g| g.grant.id);
    let stopped = ExtensionStatus::Stopped;
    let extension = extension_to_pb(&i.installation, &i.manifest, &i.artifact, grants, stopped);
    pb::InstallExtensionResponse {
        extension: extension.into(),
        plan: i.declared.as_ref().map(|d| plan_to_pb(&d.plan)).into(),
        ..Default::default()
    }
}

/// An uninstallation: the installation as it stood, its grants revoked, and the proposed
/// retirement of its workload.
fn uninstalled(u: &Uninstalled) -> pb::UninstallExtensionResponse {
    let i = &u.installation;
    let extension = pb::Extension {
        id: wire_id(IdKind::Extension, i.id),
        name: i.name.clone(),
        version: i.version.clone(),
        api_version: i.api_version.clone(),
        status: pb::ExtensionStatus::EXTENSION_STATUS_STOPPED.into(),
        organization: wire_id(IdKind::Organization, i.organization),
        project: i.project.map(|p| wire_id(IdKind::Project, p)),
        workload: wire_id(IdKind::Workload, i.workload),
        installed_by: principal::to_pb(&i.installed_by).into(),
        installed_at: timestamp(i.installed_at).into(),
        ..Default::default()
    };
    pb::UninstallExtensionResponse {
        extension: extension.into(),
        plan: u.plan.as_ref().map(plan_to_pb).into(),
        ..Default::default()
    }
}

fn upgraded(u: &Upgraded) -> pb::UpgradeExtensionResponse {
    let grants = u.grants.iter().copied();
    let stopped = ExtensionStatus::Stopped;
    let extension = extension_to_pb(&u.installation, &u.manifest, &u.artifact, grants, stopped);
    pb::UpgradeExtensionResponse {
        extension: extension.into(),
        plan: u.declared.as_ref().map(|d| plan_to_pb(&d.plan)).into(),
        issued_grants: u.issued.iter().map(|g| wire_id(IdKind::Grant, g.grant.id)).collect(),
        revoked_grants: u.revoked.iter().map(|g| wire_id(IdKind::Grant, *g)).collect(),
        from_version: u.from_version.clone(),
        ..Default::default()
    }
}

fn approved(needs: &[pb::Need]) -> Result<Vec<Need>, connectrpc::ConnectError> {
    needs
        .iter()
        .enumerate()
        .map(|(i, n)| need_from_pb(&format!("approved_needs[{i}]"), n))
        .collect::<Result<_, _>>()
        .refuse()
}

fn optional_id(
    kind: IdKind,
    field: &str,
    s: &str,
) -> Result<Option<Uuid>, connectrpc::ConnectError> {
    if s.is_empty() { Ok(None) } else { wire::id(kind, field, s).refuse().map(Some) }
}

fn install_cmd(
    req: &pb::InstallExtensionRequest,
) -> Result<InstallExtension, connectrpc::ConnectError> {
    let project = req.project.as_deref().unwrap_or_default();
    let workload_name = match req.workload_name.as_str() {
        "" => None,
        n => Some(wire::name("workload_name", n).refuse()?),
    };
    Ok(InstallExtension {
        artifact: req.artifact.clone(),
        version: req.version.clone(),
        organization: wire::id(IdKind::Organization, "organization", &req.organization).refuse()?,
        project: optional_id(IdKind::Project, "project", project)?,
        workload_project: optional_id(IdKind::Project, "workload_project", &req.workload_project)?,
        workload_name,
        approved_needs: approved(&req.approved_needs)?,
        config: map_from_pb("config", &req.config).refuse()?,
        dry_run: req.dry_run,
    })
}

impl pb::ExtensionService for Api {
    async fn install_extension(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::InstallExtensionRequest>,
    ) -> ServiceResult<pb::InstallExtensionResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let cmd = install_cmd(&req)?;
        let encoder = |i: &Installed| encode(&installed(i));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.install_extension(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, installed)
    }

    async fn get_extension(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetExtensionRequest>,
    ) -> ServiceResult<pb::GetExtensionResponse> {
        let caller = self.caller(&ctx).await?;
        let id = wire::id(IdKind::Extension, "id", request.id).refuse()?;
        let view = self.app.get_extension(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::GetExtensionResponse {
            extension: view_to_pb(&view).into(),
            ..Default::default()
        })
    }

    async fn list_extensions(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListExtensionsRequest>,
    ) -> ServiceResult<pb::ListExtensionsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Extension)?;
        let org = wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let listed = self.app.list_extensions(&caller, org, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListExtensionsResponse {
            extensions: listed.items.iter().map(view_to_pb).collect(),
            page: next_page(listed.next, IdKind::Extension).into(),
            ..Default::default()
        })
    }

    async fn uninstall_extension(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::UninstallExtensionRequest>,
    ) -> ServiceResult<pb::UninstallExtensionResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let id = wire::id(IdKind::Extension, "id", &req.id).refuse()?;
        let encoder = |u: &Uninstalled| encode(&uninstalled(u));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.uninstall_extension(&caller, &idem, id).await.map_err(error::from_app)?;
        respond(reply, uninstalled)
    }

    async fn upgrade_extension(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::UpgradeExtensionRequest>,
    ) -> ServiceResult<pb::UpgradeExtensionResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let key = mutation_key(req.mutation.as_option())?;
        let cmd = UpgradeExtension {
            id: wire::id(IdKind::Extension, "id", &req.id).refuse()?,
            version: req.version.clone(),
            approved_needs: approved(&req.approved_needs)?,
            dry_run: req.dry_run,
        };
        let encoder = |u: &Upgraded| encode(&upgraded(u));
        let idem = Idempotent { key, encode: &encoder };
        let reply =
            self.app.upgrade_extension(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, upgraded)
    }
}
