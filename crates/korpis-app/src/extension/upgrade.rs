// SPDX-License-Identifier: AGPL-3.0-or-later

//! Upgrading an installation in place (§9 question 6 of docs/16-extensions.md). The new version
//! of the same artifact is resolved under the trust policy and checked against the contract; the
//! upgrader approves its needs exactly as an installer does; each grant the installation holds
//! that a need still asks for is kept, a need no grant serves gets a grant attenuated from the
//! upgrader's, and a grant no need asks for is revoked; provides and subscriptions follow the
//! new document, keeping what is unchanged; and the new digest is declared on the same workload,
//! so the subject `extension:<id>` and everything recorded against it stay. One transaction.

use super::grants::{NeedGrant, Place};
use super::install::{Target, issued_effect, provider_taken, subscriptions};
use super::uninstall::{gone, revoke_all};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::Page;
use crate::ports::{Clock, RecipeSource, Store, Tx};
use crate::workload::{DeclareIntent, DeclareTarget, Declared};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{GrantRecord, Kind, ObjectRef, Scope};
use korpis_core::extension::{ExtensionManifest, Installation, Need};
use korpis_core::recipe::RecipeRef;
use korpis_core::workload::Workload;
use korpis_core::workload::declare::Declaration;
use uuid::Uuid;

const OPERATION: &str = "extension.upgrade";

/// An upgrade to make.
#[derive(Debug, Clone, PartialEq)]
pub struct UpgradeExtension {
    /// The installation.
    pub id: Uuid,
    /// A tag, or a digest, of the artifact it was installed from.
    pub version: String,
    /// The needs the upgrader approves: exactly the new document's.
    pub approved_needs: Vec<Need>,
    /// Resolve and check everything, write nothing, and answer with what would change.
    pub dry_run: bool,
}

/// An upgrade made, or, for a dry run, what it would be.
#[derive(Debug, Clone, PartialEq)]
pub struct Upgraded {
    /// The version it was upgraded from.
    pub from_version: String,
    /// The installation, as upgraded.
    pub installation: Installation,
    /// What the new document declares.
    pub manifest: ExtensionManifest,
    /// The new artifact, by digest, with what vouched for it.
    pub artifact: RecipeRef,
    /// The grants issued for needs no held grant serves.
    pub issued: Vec<GrantRecord>,
    /// The grants revoked because no need asks for them any more.
    pub revoked: Vec<Uuid>,
    /// Every grant it holds once upgraded: those kept, then those issued.
    pub grants: Vec<Uuid>,
    /// Its workload's new declaration; none for a dry run.
    pub declared: Option<Declared>,
}

/// What an upgrade does to the installation's grants.
struct GrantChange {
    keep: Vec<Uuid>,
    issue: Vec<NeedGrant>,
    revoke: Vec<GrantRecord>,
}

/// Whether `held` is the grant `need` asks for over `place`.
fn serves(held: &GrantRecord, need: &Need, place: &Place<'_>) -> bool {
    held.revoked_at.is_none()
        && held.grant.actions == need.actions.iter().cloned().collect()
        && held.grant.scope == Scope::Object(place.object(need.scope))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Upgrades an installation: needs `extension.install` where it is installed (and over the
    /// organization when it provides anything), as installing it did.
    pub async fn upgrade_extension(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Upgraded>,
        cmd: UpgradeExtension,
    ) -> Result<Reply<Upgraded>, AppError> {
        if !cmd.dry_run
            && let Some(r) = self.replay(caller, idem, OPERATION).await?
        {
            return Ok(Reply::Replayed(r));
        }
        let (old, target, workload) = self.upgrade_target(caller, cmd.id).await?;
        let (artifact, manifest, version) =
            self.upgrade_artifact(caller, &old, &target, &cmd).await?;
        let to = Installation {
            version,
            api_version: manifest.api_version.clone(),
            artifact_digest: artifact.digest.clone(),
            ..old.clone()
        };
        let place =
            Place { organization: old.organization, project: old.project, workload: &workload };
        let change = self.grant_change(caller, &to, &manifest, &place).await?;
        let mut upgraded = Upgraded {
            from_version: old.version.clone(),
            installation: to,
            issued: change.issue.iter().map(|g| g.record.clone()).collect(),
            revoked: change.revoke.iter().map(|g| g.grant.id).collect(),
            grants: change
                .keep
                .iter()
                .copied()
                .chain(change.issue.iter().map(|g| g.record.grant.id))
                .collect(),
            manifest,
            artifact,
            declared: None,
        };
        if cmd.dry_run {
            return Ok(Reply::Fresh(upgraded));
        }
        let declare = redeclaration(&old, &workload, &upgraded.artifact, self).await?;
        let digest = Some(upgraded.artifact.digest.clone());
        let mut prepared = Box::pin(self.prepare_declaration(caller, declare, digest)).await?;
        let mut tx = self.store.begin().await?;
        self.write_prepared(&mut tx, caller, &mut prepared).await?;
        self.write_upgrade(&mut tx, caller, (&old, &upgraded), &change, target.permit.grant)
            .await?;
        upgraded.declared = Some(prepared.into_declared());
        finish(tx, caller, idem, OPERATION, upgraded).await
    }

    /// The installation, still installed, where it is, authorized, and its workload.
    async fn upgrade_target(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<(Installation, Target, Workload), AppError> {
        let old =
            self.store.extension(id).await?.ok_or(AppError::NotFound { kind: "extension" })?;
        let object = match old.project {
            Some(p) => ObjectRef::new(Kind::Project, p),
            None => ObjectRef::new(Kind::Organization, old.organization),
        };
        let permit = self.authorize(caller, "extension.install", object).await?;
        if !old.is_installed() {
            return Err(gone());
        }
        let workload = self.store.workload(old.workload).await?.filter(|w| w.deleted_at.is_none());
        let workload = workload.ok_or_else(|| {
            AppError::conflict("extension.retired", "its workload is retired; reinstall it")
        })?;
        let target = Target {
            organization: old.organization,
            project: old.project,
            workload_project: workload.project,
            permit,
        };
        Ok((old, target, workload))
    }

    /// The new version, resolved under the trust policy, of the same extension, which this
    /// control plane can serve, with exactly its needs approved: its artifact, manifest, and
    /// document version.
    async fn upgrade_artifact(
        &self,
        caller: &Caller,
        old: &Installation,
        target: &Target,
        cmd: &UpgradeExtension,
    ) -> Result<(RecipeRef, ExtensionManifest, String), AppError> {
        let (artifact, described) =
            self.resolve_trusted(&old.artifact_name, &cmd.version, old.organization).await?;
        let manifest = described
            .extension
            .ok_or_else(|| AppError::invalid("version", "a recipe, not an extension"))?;
        if described.recipe.metadata.name != old.name {
            let why =
                format!("a version of {}, not of {}", old.name, described.recipe.metadata.name);
            return Err(AppError::invalid("version", why));
        }
        if artifact.digest == old.artifact_digest {
            return Err(AppError::invalid("version", "the version installed already"));
        }
        manifest.compatible().map_err(|e| AppError::Unsatisfiable {
            reason: "extension.incompatible",
            message: e.to_string(),
        })?;
        self.provider_authority(caller, target, &manifest).await?;
        if !cmd.dry_run && cmd.approved_needs != manifest.needs {
            let why = "must be exactly the new document's needs, which a dry run returns";
            return Err(AppError::invalid("approved_needs", why));
        }
        Ok((artifact, manifest, described.recipe.metadata.version))
    }

    /// Which held grants the new needs keep, which they no longer ask for, and the grants the
    /// needs no held grant serves get, attenuated from the upgrader's.
    async fn grant_change(
        &self,
        caller: &Caller,
        to: &Installation,
        manifest: &ExtensionManifest,
        place: &Place<'_>,
    ) -> Result<GrantChange, AppError> {
        let key = to.principal().key();
        let mut held = self.store.grants_held(&key, Page::new(None, Page::MAX)).await?;
        held.retain(|g| g.revoked_at.is_none());
        let (mut keep, mut unserved) = (Vec::new(), Vec::new());
        for (i, need) in manifest.needs.iter().enumerate() {
            match held.iter().position(|g| serves(g, need, place)) {
                Some(kept) => keep.push(held.swap_remove(kept).grant.id),
                None => unserved.push((i, need)),
            }
        }
        let issue = self.need_grants(caller, (to.id, to.name.as_str()), &unserved, place).await?;
        Ok(GrantChange { keep, issue, revoke: held })
    }

    /// Writes everything but the workload's declaration in `tx`.
    async fn write_upgrade<X: Tx>(
        &self,
        tx: &mut X,
        caller: &Caller,
        (old, new): (&Installation, &Upgraded),
        change: &GrantChange,
        under: Uuid,
    ) -> Result<(), AppError> {
        let i = &new.installation;
        if !tx.upgrade_extension(&old.artifact_digest, i).await? {
            let why = "the installation changed meanwhile; read it and upgrade again";
            return Err(AppError::conflict("extension.changed", why));
        }
        let now = self.clock.now();
        revoke_all(tx, caller, &change.revoke, under, now).await?;
        for NeedGrant { record, parent } in &change.issue {
            tx.insert_grant(record).await?;
            tx.append_effect(&issued_effect(caller, record, parent.id)).await?;
        }
        tx.replace_provides(i.id, i.organization, &new.manifest.provides)
            .await
            .map_err(provider_taken)?;
        super::provider::fail_in_flight(tx, now).await?;
        self.replace_subscriptions(tx, i, &new.manifest).await?;
        tx.append_effect(&upgrade_effect(caller, old, new, under)).await?;
        Ok(())
    }

    /// Keeps each subscription whose filter the new document still declares, with its
    /// position; deletes the rest; and starts the new ones at the end of the log.
    async fn replace_subscriptions<X: Tx>(
        &self,
        tx: &mut X,
        i: &Installation,
        manifest: &ExtensionManifest,
    ) -> Result<(), AppError> {
        let key = i.principal().key();
        let mut current = self.store.subscriptions_of(&key, Page::new(None, Page::MAX)).await?;
        let start = self.store.last_position().await?;
        let created = Installation { installed_at: self.clock.now(), ..i.clone() };
        for wanted in subscriptions(&created, manifest, start) {
            match current.iter().position(|s| s.filter == wanted.filter) {
                Some(kept) => drop(current.swap_remove(kept)),
                None => tx.insert_subscription(&wanted).await?,
            }
        }
        for gone in current {
            tx.delete_subscription(gone.id).await?;
        }
        Ok(())
    }
}

/// The workload's declaration at the new digest: the same workload, with the configuration it
/// runs with now, which the new document's schema must still accept.
async fn redeclaration<S: Store, C: Clock, R: RecipeSource>(
    old: &Installation,
    workload: &Workload,
    artifact: &RecipeRef,
    app: &App<S, C, R>,
) -> Result<DeclareIntent, AppError> {
    let intent = app.store.intent(workload.intent).await?;
    let intent = intent.ok_or_else(|| AppError::Internal("its workload has no intent".into()))?;
    Ok(DeclareIntent {
        target: DeclareTarget::Existing {
            workload: workload.id,
            expected_version: workload.intent_version,
        },
        recipe_name: old.artifact_name.clone(),
        recipe_version: artifact.digest.clone(),
        declaration: Declaration { config: intent.body.config, ..Declaration::default() },
        dry_run: false,
        propose: false,
        set: None,
    })
}

fn upgrade_effect(caller: &Caller, old: &Installation, new: &Upgraded, under: Uuid) -> Effect {
    let i = &new.installation;
    let mut e = Effect::new(caller.principal.clone(), OPERATION, Outcome::Succeeded)
        .under(under)
        .after(details([
            ("extension", i.id.to_string().into()),
            ("from_version", old.version.clone().into()),
            ("version", i.version.clone().into()),
            ("from_artifact", old.artifact_digest.clone().into()),
            ("artifact", i.artifact_digest.clone().into()),
            ("issued", new.issued.len().into()),
            ("revoked", new.revoked.len().into()),
        ]));
    e.workload = Some(i.workload);
    e
}
