// SPDX-License-Identifier: AGPL-3.0-or-later

//! Uninstalling an extension (§4 of docs/16-extensions.md): its grants are revoked, which revokes
//! everything descended from them, its record says when, and the retirement of its workload is
//! proposed, all in one transaction. The retirement waits for a human like every other (§3.3 of
//! docs/01-model.md); the extension can obtain nothing meanwhile, because it holds no grant.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::Page;
use crate::ports::{
    Clock, EffectWrite, ExtensionWrite, ProviderCallWrite, ProviderWrite, RecipeSource,
    RetirementWrite, Store, SubscriptionWrite, Tx,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{GrantRecord, Kind, ObjectRef};
use korpis_core::extension::{CallFailure, CallOutcome, Installation};
use korpis_core::workload::Plan;
use std::time::SystemTime;
use uuid::Uuid;

const OPERATION: &str = "extension.uninstall";

/// An installation ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Uninstalled {
    /// The installation, as uninstalled.
    pub installation: Installation,
    /// The proposed retirement of its workload; none if the workload is already gone or
    /// being retired.
    pub plan: Option<Plan>,
    /// How many of its grants were revoked.
    pub revoked: u64,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Uninstalls an extension: needs `extension.uninstall` where it is installed, and
    /// `workload.delete` on its workload to propose that workload's retirement.
    pub async fn uninstall_extension(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Uninstalled>,
        id: Uuid,
    ) -> Result<Reply<Uninstalled>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let mut installation =
            self.store.extension(id).await?.ok_or(AppError::NotFound { kind: "extension" })?;
        let target = match installation.project {
            Some(p) => ObjectRef::new(Kind::Project, p),
            None => ObjectRef::new(Kind::Organization, installation.organization),
        };
        let permit = self.authorize(caller, "extension.uninstall", target).await?;
        if !installation.is_installed() {
            return Err(gone());
        }
        let workload = self.store.workload(installation.workload).await?;
        let retire = match workload.filter(|w| w.deleted_at.is_none()) {
            Some(w) => {
                let object = ObjectRef::new(Kind::Workload, w.id);
                let (wp, proposing) =
                    self.authorize_declaration(caller, "workload.delete", object).await?;
                let plan = self.retire_plan(caller, &w, &proposing, wp.grant).await?;
                Some((w, plan, wp.grant))
            }
            None => None,
        };
        let key = installation.principal().key();
        let held = self.store.grants_held(&key, Page::new(None, Page::MAX)).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let revoked = revoke_all(&mut tx, caller, &held, permit.grant, now).await?;
        if !tx.uninstall_extension(id, now).await? {
            return Err(gone());
        }
        let ended = tx.delete_extension_subscriptions(id).await?;
        tx.delete_provides(id).await?;
        super::provider::fail_in_flight(&mut tx, now).await?;
        let unknown = CallOutcome::Failed(CallFailure::unknown());
        tx.fail_calls_without_provider(&unknown, now).await?;
        installation.uninstalled_at = Some(now);
        let mut plan = None;
        if let Some((w, p, grant)) = retire
            && !tx.retiring(w.id).await?
        {
            self.write_retirement(&mut tx, caller, &w, &p, grant).await?;
            plan = Some(p);
        }
        let mut effect = Effect::new(caller.principal.clone(), OPERATION, Outcome::Succeeded)
            .under(permit.grant)
            .after(details([
                ("extension", id.to_string().into()),
                ("revoked_grants", revoked.into()),
                ("ended_subscriptions", ended.into()),
            ]));
        effect.workload = Some(installation.workload);
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, OPERATION, Uninstalled { installation, plan, revoked }).await
    }
}

fn gone() -> AppError {
    AppError::conflict("extension.uninstalled", "the extension is uninstalled already")
}

/// Revokes every grant in `held` not revoked already, each with its effect; how many.
async fn revoke_all<X: Tx>(
    tx: &mut X,
    caller: &Caller,
    held: &[GrantRecord],
    under: Uuid,
    now: SystemTime,
) -> Result<u64, AppError> {
    let mut n = 0;
    for g in held.iter().filter(|g| g.revoked_at.is_none()) {
        tx.revoke_grant(g.grant.id, now).await?;
        let cascaded = tx.descendant_count(g.grant.id).await?;
        let effect = Effect::new(caller.principal.clone(), "grant.revoke", Outcome::Succeeded)
            .under(under)
            .after(details([
                ("grant", g.grant.id.to_string().into()),
                ("cascaded", cascaded.into()),
            ]));
        tx.append_effect(&effect).await?;
        n += 1;
    }
    Ok(n)
}
