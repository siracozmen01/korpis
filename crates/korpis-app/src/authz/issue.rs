// SPDX-License-Identifier: AGPL-3.0-or-later

//! Issuing a grant: attenuating one the caller holds (§3.2 of docs/08-identity.md), to a named
//! holder or to an API token created to hold it (§3.7).

use super::Snapshot;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    Clock, CredentialWrite, EffectWrite, GrantWrite, RecipeSource, Store, StoreError, Unique,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::credential::{ApiToken, Secret};
use korpis_core::authz::evaluate::{IssueError, issue};
use korpis_core::authz::{Action, Conditions, Delegation, Grant, GrantRecord, Scope};
use korpis_core::principal::Principal;
use std::collections::BTreeSet;
use uuid::Uuid;

const OPERATION: &str = "authz.issue_grant";

/// The longest label a grant may carry, in characters.
pub const MAX_LABEL: usize = 200;

/// Who will hold a grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// A principal that exists, or is named by an identity provider.
    Principal(Principal),
    /// An API token created in the same transaction, which exists only to hold the grant.
    NewToken,
}

/// A grant to issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueGrant {
    /// The grant to attenuate, which the caller must hold; with none named, the one grant the
    /// caller holds that the attenuation is valid from.
    pub parent: Option<Uuid>,
    /// Who will hold the new grant.
    pub holder: Holder,
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
    /// What it is for, for people; required for a new token, which it names.
    pub label: String,
}

/// An issued grant, and the secret of the token created to hold it, if one was.
#[derive(Debug)]
pub struct Issued {
    /// The grant.
    pub record: GrantRecord,
    /// The new token's secret, answered once and stored only as its digest.
    pub secret: Option<Secret>,
}

fn issue_error(e: &IssueError) -> AppError {
    match e {
        IssueError::NotHolder => AppError::invalid("parent", "the caller does not hold it"),
        other => AppError::invalid("grant", other.to_string()),
    }
}

/// Checks what the request asks for before anything is read.
fn validate(cmd: &IssueGrant) -> Result<(), AppError> {
    if cmd.link_form {
        // Links are redeemed online with a use count; that path arrives with them.
        return Err(AppError::NotYet { what: "share links", phase: 1 });
    }
    if cmd.actions.is_empty() {
        return Err(AppError::invalid("actions", "at least one action"));
    }
    if cmd.label.chars().count() > MAX_LABEL || cmd.label.chars().any(char::is_control) {
        return Err(AppError::invalid("label", "at most 200 characters, none of them control"));
    }
    match &cmd.holder {
        Holder::Principal(p) => {
            p.validate().map_err(|e| AppError::invalid("subject", e.to_string()))
        }
        Holder::NewToken if cmd.label.trim().is_empty() => {
            Err(AppError::invalid("label", "a new token is named by its label"))
        }
        Holder::NewToken => Ok(()),
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Issues a grant attenuated from one the caller holds.
    pub async fn issue_grant(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Issued>,
        cmd: IssueGrant,
    ) -> Result<Reply<Issued>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        validate(&cmd)?;
        let now = self.clock.now();
        let (holder, token) = match cmd.holder {
            Holder::Principal(p) => (p, None),
            Holder::NewToken => {
                let token = ApiToken {
                    id: Uuid::now_v7(),
                    name: cmd.label.clone(),
                    expires_at: cmd.conditions.expires_at,
                };
                let secret = Secret::generate().map_err(|e| AppError::Internal(e.to_string()))?;
                (Principal::Token(token.id), Some((token, secret)))
            }
        };
        let mut record = GrantRecord {
            grant: Grant {
                id: Uuid::now_v7(),
                subject: holder.subject(),
                actions: cmd.actions,
                scope: cmd.scope,
                conditions: cmd.conditions,
                parent: cmd.parent,
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
            holder,
            issued_by: caller.principal.clone(),
            issued_at: now,
            revoked_at: None,
            suspended_at: None,
            label: cmd.label,
        };
        let parent = self.parent_for(caller, &mut record.grant).await?;
        self.record_issue(caller, idem, record, &parent, token).await
    }

    /// Writes an issued grant, the token holding it when there is one, and its effect.
    async fn record_issue(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Issued>,
        record: GrantRecord,
        parent: &Grant,
        token: Option<(ApiToken, Secret)>,
    ) -> Result<Reply<Issued>, AppError> {
        let mut tx = self.store.begin().await?;
        // A child takes its uses from a use-limited parent under compare-and-swap (§6.2), so two
        // concurrent attenuations cannot both spend the same uses.
        if let (Some(max), Some(taken)) =
            (parent.conditions.max_uses, record.grant.conditions.max_uses)
            && !tx.take_uses(parent.id, taken, max).await?
        {
            return Err(AppError::conflict("grant.uses_taken", "the parent's uses changed"));
        }
        if let Some((t, secret)) = &token {
            tx.insert_token(t, &secret.digest(), &caller.principal).await?;
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
                ("label", record.label.clone().into()),
            ]));
        tx.append_effect(&effect).await?;
        let secret = token.map(|(_, s)| s);
        finish(tx, caller, idem, OPERATION, Issued { record, secret }).await
    }

    /// The parent `child` is attenuated from, checked: the one it names, or else the one grant
    /// the caller holds that it is valid from, which is then named on it.
    async fn parent_for(&self, caller: &Caller, child: &mut Grant) -> Result<Grant, AppError> {
        if child.parent.is_some() {
            return self.check_issue(caller, child).await;
        }
        let held = self.store.chains_held_by(&caller.subject.key).await?;
        let leaves: Vec<Uuid> = held.iter().filter_map(|c| c.first().map(|r| r.grant.id)).collect();
        let (mut valid, mut refused) = (Vec::new(), None);
        for leaf in &leaves {
            child.parent = Some(*leaf);
            match self.check_issue(caller, child).await {
                Ok(p) => valid.push(p),
                Err(e @ (AppError::Invalid { .. } | AppError::NotFound { .. })) => {
                    refused = Some(e);
                }
                Err(other) => return Err(other),
            }
        }
        match (valid.pop(), valid.len(), refused) {
            (Some(p), 0, _) => {
                child.parent = Some(p.id);
                Ok(p)
            }
            (Some(_), n, _) => Err(AppError::invalid(
                "parent",
                format!("{} grants you hold can be attenuated into it; name one", n + 1),
            )),
            (None, _, Some(e)) if leaves.len() == 1 => Err(e),
            (None, ..) => Err(AppError::invalid(
                "parent",
                format!("none of the {} grants you hold can be attenuated into it", leaves.len()),
            )),
        }
    }

    /// Checks the attenuation against the parent's chain as it stands; returns the parent.
    pub(crate) async fn check_issue(
        &self,
        caller: &Caller,
        child: &Grant,
    ) -> Result<Grant, AppError> {
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
