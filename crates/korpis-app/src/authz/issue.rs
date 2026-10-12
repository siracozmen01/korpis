// SPDX-License-Identifier: AGPL-3.0-or-later

//! Issuing a grant: attenuating one the caller holds (§3.4 of docs/08-identity.md).

use super::Snapshot;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, GrantWrite, RecipeSource, Store, StoreError, Unique};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::evaluate::{IssueError, issue};
use korpis_core::authz::{Action, Conditions, Delegation, Grant, GrantRecord, Scope};
use korpis_core::principal::Principal;
use std::collections::BTreeSet;
use uuid::Uuid;

const OPERATION: &str = "authz.issue_grant";

/// A grant to issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueGrant {
    /// The grant to attenuate; the caller must hold it.
    pub parent: Uuid,
    /// Who will hold the new grant.
    pub holder: Principal,
    /// What it permits; at least one.
    pub actions: BTreeSet<Action>,
    /// Where.
    pub scope: Scope,
    /// Under which conditions.
    pub conditions: Conditions,
    /// What the holder may pass on.
    pub delegation: Delegation,
    /// A share link rather than a named holder (§6.1 of docs/08-identity.md).
    pub link_form: bool,
}

fn issue_error(e: &IssueError) -> AppError {
    match e {
        IssueError::NotHolder => AppError::invalid("parent", "the caller does not hold it"),
        other => AppError::invalid("grant", other.to_string()),
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Issues a grant attenuated from one the caller holds.
    pub async fn issue_grant(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, GrantRecord>,
        cmd: IssueGrant,
    ) -> Result<Reply<GrantRecord>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        if cmd.link_form {
            // Links are redeemed online with a use count; that path arrives with them.
            return Err(AppError::NotYet { what: "share links", phase: 1 });
        }
        if cmd.actions.is_empty() {
            return Err(AppError::invalid("actions", "at least one action"));
        }
        cmd.holder.validate().map_err(|e| AppError::invalid("subject", e.to_string()))?;
        let now = self.clock.now();
        let record = GrantRecord {
            grant: Grant {
                id: Uuid::now_v7(),
                subject: cmd.holder.subject(),
                actions: cmd.actions,
                scope: cmd.scope,
                conditions: cmd.conditions,
                parent: Some(cmd.parent),
                delegation: cmd.delegation,
                link_form: false,
                uses: 0,
                revoked: false,
                suspended: false,
                // Identity bindings arrive with external identity (Phase 2); until then no
                // binding can override the irreversible-action default, so nothing claims it.
                irreversible_override: false,
                root: false,
            },
            holder: cmd.holder,
            issued_by: caller.principal.clone(),
            issued_at: now,
            revoked_at: None,
            suspended_at: None,
        };
        let parent = self.check_issue(caller, &record.grant).await?;

        let mut tx = self.store.begin().await?;
        // A child takes its uses from a use-limited parent under compare-and-swap (§6.2), so two
        // concurrent attenuations cannot both spend the same uses.
        if let (Some(max), Some(taken)) =
            (parent.conditions.max_uses, record.grant.conditions.max_uses)
            && !tx.take_uses(parent.id, taken, max).await?
        {
            return Err(AppError::conflict("grant.uses_taken", "the parent's uses changed"));
        }
        tx.insert_grant(&record).await.map_err(|e| match e {
            StoreError::Duplicate(Unique::Other) => {
                AppError::conflict("grant.exists", "the grant exists")
            }
            other => other.into(),
        })?;
        let effect = Effect::new(caller.principal.clone(), "grant.issued", Outcome::Succeeded)
            .under(parent.id)
            .after(details([
                ("grant", record.grant.id.to_string().into()),
                ("parent", parent.id.to_string().into()),
                ("holder", record.holder.key().into()),
            ]));
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, OPERATION, record).await
    }

    /// Checks the attenuation against the parent's chain as it stands; returns the parent.
    async fn check_issue(&self, caller: &Caller, child: &Grant) -> Result<Grant, AppError> {
        let parent_id = child.parent.ok_or(AppError::invalid("parent", "required"))?;
        let chain: Vec<Grant> =
            self.store.grant_chain(parent_id).await?.into_iter().map(|r| r.grant).collect();
        let Some(parent) = chain.first().cloned() else {
            return Err(AppError::NotFound { kind: "grant" });
        };
        let mut snapshot = Snapshot::default();
        for g in chain.iter().chain(std::iter::once(child)) {
            snapshot.load_grant(&self.store, g).await?;
        }
        issue(&caller.subject, &chain, child, false, &snapshot, self.clock.now())
            .map_err(|e| issue_error(&e))?;
        Ok(parent)
    }
}
