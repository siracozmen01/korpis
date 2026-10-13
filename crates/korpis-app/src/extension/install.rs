// SPDX-License-Identifier: AGPL-3.0-or-later

//! Installing an extension (§4 of docs/16-extensions.md). Its artifact is resolved under the
//! organization's trust policy; the installer must hold `extension.install` where it is installed,
//! and over the organization when it provides anything (§4.1); the needs approved must be exactly
//! the document's; and its workload, its grants, its record, and their effects are written in one
//! transaction, so an installation either exists whole or not at all.

use super::grants::{NeedGrant, Place};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, RecipeSource, Store, StoreError, SubscriptionWrite, Tx, Unique};
use crate::workload::{DeclareIntent, DeclareTarget, Declared, NewWorkload};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{GrantRecord, Kind, ObjectRef, Permit};
use korpis_core::event::Subscription;
use korpis_core::extension::{ExtensionManifest, Installation, Need, ProviderType};
use korpis_core::labels::Labels;
use korpis_core::recipe::RecipeRef;
use korpis_core::tenancy::Name;
use korpis_core::value::ValueMap;
use korpis_core::workload::PowerState;
use korpis_core::workload::declare::Declaration;
use uuid::Uuid;

const OPERATION: &str = "extension.install";

/// An installation to make.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallExtension {
    /// The artifact's repository.
    pub artifact: String,
    /// A tag, or a digest.
    pub version: String,
    /// The organization it is installed into.
    pub organization: Uuid,
    /// The project, when it is installed into one rather than the whole organization.
    pub project: Option<Uuid>,
    /// The project its workload runs in; the target project when one is named.
    pub workload_project: Option<Uuid>,
    /// Its workload's name; the extension's name when absent.
    pub workload_name: Option<Name>,
    /// The needs the installer approves: exactly the document's.
    pub approved_needs: Vec<Need>,
    /// Its configuration, validated against its document's schema.
    pub config: ValueMap,
    /// Resolve and check everything, write nothing, and answer with the manifest.
    pub dry_run: bool,
}

/// An installation made, or, for a dry run, what it would be.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    /// The installation.
    pub installation: Installation,
    /// What its document declares.
    pub manifest: ExtensionManifest,
    /// Its artifact, by digest, with what vouched for it.
    pub artifact: RecipeRef,
    /// One grant per approved need.
    pub grants: Vec<GrantRecord>,
    /// Its workload's declaration; none for a dry run.
    pub declared: Option<Declared>,
}

/// The artifact, resolved and read.
struct Resolved {
    artifact: RecipeRef,
    manifest: ExtensionManifest,
    /// The document's `metadata.name`, which names its actions and, by default, its workload.
    name: Name,
    /// The document's `metadata.version`.
    version: String,
}

/// Where an installation goes, checked and authorized.
struct Target {
    organization: Uuid,
    project: Option<Uuid>,
    workload_project: Uuid,
    permit: Permit,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Installs an extension.
    pub async fn install_extension(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Installed>,
        cmd: InstallExtension,
    ) -> Result<Reply<Installed>, AppError> {
        if !cmd.dry_run
            && let Some(r) = self.replay(caller, idem, OPERATION).await?
        {
            return Ok(Reply::Replayed(r));
        }
        let (target, resolved) = self.checked(caller, &cmd).await?;
        let Resolved { artifact, manifest, name, version } = resolved;
        let declare = workload_declaration(&cmd, &target, &name);
        let digest = Some(artifact.digest.clone());
        let mut prepared = Box::pin(self.prepare_declaration(caller, declare, digest)).await?;
        let id = Uuid::now_v7();
        let place = Place {
            organization: target.organization,
            project: target.project,
            workload: prepared.workload(),
        };
        let grants = self.need_grants(caller, (id, name.as_str()), &manifest.needs, &place).await?;
        let installation = Installation {
            id,
            organization: target.organization,
            project: target.project,
            name: name.as_str().to_owned(),
            version,
            api_version: manifest.api_version.clone(),
            workload: prepared.workload().id,
            artifact_name: cmd.artifact.clone(),
            artifact_digest: artifact.digest.clone(),
            installed_by: caller.principal.clone(),
            installed_at: self.clock.now(),
            uninstalled_at: None,
        };
        let records = grants.iter().map(|g| g.record.clone()).collect();
        let mut installed =
            Installed { installation, manifest, artifact, grants: records, declared: None };
        if cmd.dry_run {
            return Ok(Reply::Fresh(installed));
        }
        let start = self.store.last_position().await?;
        let mut tx = self.store.begin().await?;
        self.write_prepared(&mut tx, caller, &mut prepared).await?;
        write_installation(&mut tx, caller, &installed, &grants, target.permit.grant).await?;
        for s in subscriptions(&installed.installation, &installed.manifest, start) {
            tx.insert_subscription(&s).await?;
        }
        installed.declared = Some(prepared.into_declared());
        finish(tx, caller, idem, OPERATION, installed).await
    }

    /// Everything an install is refused for before anything is computed: where it goes, who
    /// installs it, what it is, whether it is installed there already, and what was approved.
    async fn checked(
        &self,
        caller: &Caller,
        cmd: &InstallExtension,
    ) -> Result<(Target, Resolved), AppError> {
        let target = self.install_target(caller, cmd).await?;
        let resolved = self.extension_artifact(cmd).await?;
        self.provider_authority(caller, &target, &resolved.manifest).await?;
        // Checked here so the answer names the installation rather than its workload's name;
        // the store's unique index still decides between two installs racing.
        let name = resolved.name.as_str();
        if self
            .store
            .installed_extension(target.organization, target.project, name)
            .await?
            .is_some()
        {
            return Err(exists());
        }
        if !cmd.dry_run && cmd.approved_needs != resolved.manifest.needs {
            let why = "must be exactly the document's needs, which a dry run returns";
            return Err(AppError::invalid("approved_needs", why));
        }
        Ok((target, resolved))
    }

    /// Checks where the installation goes and that the installer may install there.
    async fn install_target(
        &self,
        caller: &Caller,
        cmd: &InstallExtension,
    ) -> Result<Target, AppError> {
        let in_org = |p: Uuid, field: &'static str| async move {
            match self.store.project(p).await? {
                Some(project) if project.organization == cmd.organization => Ok(p),
                _ => Err(AppError::invalid(field, "a project of the organization")),
            }
        };
        let project = match cmd.project {
            Some(p) => Some(in_org(p, "project").await?),
            None => None,
        };
        let workload_project = match (project, cmd.workload_project) {
            (Some(p), None) => p,
            (Some(p), Some(w)) if w == p => p,
            (Some(_), Some(_)) => {
                let why = "an extension installed into a project runs in that project";
                return Err(AppError::invalid("workload_project", why));
            }
            (None, Some(w)) => in_org(w, "workload_project").await?,
            (None, None) => return Err(AppError::invalid("workload_project", "required")),
        };
        let object = match project {
            Some(p) => ObjectRef::new(Kind::Project, p),
            None => ObjectRef::new(Kind::Organization, cmd.organization),
        };
        let permit = self.authorize(caller, "extension.install", object).await?;
        Ok(Target { organization: cmd.organization, project, workload_project, permit })
    }

    /// The artifact, resolved under the organization's trust policy, and its manifest.
    async fn extension_artifact(&self, cmd: &InstallExtension) -> Result<Resolved, AppError> {
        let (artifact, described) =
            self.resolve_trusted(&cmd.artifact, &cmd.version, cmd.organization).await?;
        let Some(manifest) = described.extension else {
            return Err(AppError::invalid("artifact", "a recipe, not an extension"));
        };
        let metadata = described.recipe.metadata;
        let name = Name::parse(metadata.name)
            .map_err(|e| AppError::invalid("artifact", format!("its name: {e}")))?;
        Ok(Resolved { artifact, manifest, name, version: metadata.version })
    }

    /// A provider is infrastructure for the whole organization (§4.1 of
    /// docs/16-extensions.md): installing one needs `extension.install` over the organization,
    /// and an identity provider is installed into the organization it authenticates into.
    async fn provider_authority(
        &self,
        caller: &Caller,
        target: &Target,
        manifest: &ExtensionManifest,
    ) -> Result<(), AppError> {
        if manifest.provides.is_empty() || target.project.is_none() {
            return Ok(());
        }
        if manifest.provides.iter().any(|p| p.kind == ProviderType::Identity) {
            let why = "an identity provider is installed into the organization it authenticates";
            return Err(AppError::invalid("project", why));
        }
        let organization = ObjectRef::new(Kind::Organization, target.organization);
        self.authorize(caller, "extension.install", organization).await.map(|_| ())
    }
}

/// The declaration of the extension's workload: its own document, pinned by the installation.
fn workload_declaration(cmd: &InstallExtension, target: &Target, name: &Name) -> DeclareIntent {
    DeclareIntent {
        target: DeclareTarget::New(NewWorkload {
            project: target.workload_project,
            name: cmd.workload_name.clone().unwrap_or_else(|| name.clone()),
            labels: Labels::default(),
            power: PowerState::Running,
        }),
        recipe_name: cmd.artifact.clone(),
        recipe_version: cmd.version.clone(),
        declaration: Declaration { config: cmd.config.clone(), ..Declaration::default() },
        dry_run: false,
        propose: false,
        set: None,
    }
}

/// Writes the installation, its grants, and their effects in `tx`.
async fn write_installation<X: Tx>(
    tx: &mut X,
    caller: &Caller,
    installed: &Installed,
    grants: &[NeedGrant],
    under: Uuid,
) -> Result<(), AppError> {
    let i = &installed.installation;
    tx.insert_extension(i).await.map_err(|e| match e {
        StoreError::Duplicate(Unique::ExtensionName) => exists(),
        other => other.into(),
    })?;
    tx.insert_provides(i.id, i.organization, &installed.manifest.provides).await.map_err(|e| {
        match e {
            StoreError::Duplicate(Unique::ProviderName) => AppError::conflict(
                "provider.taken",
                "another installation serves a provider of that type and name here",
            ),
            other => other.into(),
        }
    })?;
    for NeedGrant { record, parent } in grants {
        tx.insert_grant(record).await?;
        tx.append_effect(&issued_effect(caller, record, parent.id)).await?;
    }
    tx.append_effect(&install_effect(caller, installed, under)).await?;
    Ok(())
}

/// The subscriptions its document declares, held by the installation and starting at the end of
/// the log; installed into a project, each is narrowed to that project (§4 of
/// docs/16-extensions.md).
fn subscriptions(
    i: &Installation,
    manifest: &ExtensionManifest,
    start: u64,
) -> impl Iterator<Item = Subscription> {
    let (id, project, created) = (i.id, i.project, i.installed_at);
    manifest.subscribes.clone().into_iter().map(move |mut filter| {
        filter.projects = project.into_iter().collect();
        Subscription {
            id: Uuid::now_v7(),
            subject: korpis_core::principal::Principal::Extension(id),
            filter,
            acked: start,
            created,
            extension: Some(id),
        }
    })
}

fn exists() -> AppError {
    AppError::conflict("extension.exists", "an extension of this name is installed here already")
}

fn issued_effect(caller: &Caller, record: &GrantRecord, parent: Uuid) -> Effect {
    Effect::new(caller.principal.clone(), "grant.issued", Outcome::Succeeded).under(parent).after(
        details([
            ("grant", record.grant.id.to_string().into()),
            ("parent", parent.to_string().into()),
            ("holder", record.holder.key().into()),
            ("label", record.label.clone().into()),
        ]),
    )
}

fn install_effect(caller: &Caller, i: &Installed, grant: Uuid) -> Effect {
    let x = &i.installation;
    let mut e = Effect::new(caller.principal.clone(), "extension.install", Outcome::Succeeded)
        .under(grant)
        .after(details([
            ("extension", x.id.to_string().into()),
            ("name", x.name.clone().into()),
            ("version", x.version.clone().into()),
            ("artifact", x.artifact_digest.clone().into()),
            ("grants", i.grants.len().into()),
        ]));
    e.workload = Some(x.workload);
    e
}
