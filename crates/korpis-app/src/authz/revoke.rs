// SPDX-License-Identifier: AGPL-3.0-or-later

//! Revoking a grant (§3.5 of docs/08-identity.md). Revocation is written on one grant and
//! derived for everything below it.

use super::Snapshot;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, GrantWrite, RecipeSource, Store};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::evaluate::verify_chain;
use korpis_core::authz::{Grant, GrantRecord};
use uuid::Uuid;

const OPERATION: &str = "authz.revoke_grant";

/// A revocation's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revoked {
    /// The grant, as revoked.
    pub grant: GrantRecord,
    /// How many grants descend from it and are refused from now on.
    pub cascaded: u64,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Revokes a grant. Permitted to its holder; to anyone holding a currently valid grant above
    /// it on its chain; and to a holder of `grant.revoke` over a scope containing it.
    pub async fn revoke_grant(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Revoked>,
        grant: Uuid,
    ) -> Result<Reply<Revoked>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let records = self.store.grant_chain(grant).await?;
        let Some(mut target) = records.first().cloned() else {
            return Err(AppError::NotFound { kind: "grant" });
        };
        if target.grant.root {
            return Err(AppError::invalid(
                "grant",
                "a root grant is replaced by local root recovery, not revoked (§9.2 of \
                 docs/08-identity.md)",
            ));
        }
        let chain: Vec<Grant> = records.into_iter().map(|r| r.grant).collect();
        let authority = self.revocation_authority(caller, &chain).await?;

        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        tx.revoke_grant(grant, now).await?;
        let cascaded = tx.descendant_count(grant).await?;
        let effect = Effect::new(caller.principal.clone(), "grant.revoke", Outcome::Succeeded)
            .under(authority)
            .after(details([("grant", grant.to_string().into()), ("cascaded", cascaded.into())]));
        tx.append_effect(&effect).await?;
        target.revoked_at = Some(now);
        target.grant.revoked = true;
        finish(tx, caller, idem, OPERATION, Revoked { grant: target, cascaded }).await
    }

    /// The grant under which `caller` may revoke the leaf of `chain`.
    async fn revocation_authority(
        &self,
        caller: &Caller,
        chain: &[Grant],
    ) -> Result<Uuid, AppError> {
        let Some(target) = chain.first() else { return Err(AppError::NotFound { kind: "grant" }) };
        if target.subject == caller.subject {
            return Ok(target.id);
        }
        let mut snapshot = Snapshot::default();
        for g in chain {
            snapshot.load_grant(&self.store, g).await?;
        }
        let now = self.clock.now();
        let above = (1..chain.len()).find_map(|i| {
            let rest = chain.get(i..)?;
            let g = rest.first()?;
            (g.subject == caller.subject && verify_chain(rest, &snapshot, now).is_ok())
                .then_some(g.id)
        });
        match above {
            Some(id) => Ok(id),
            None => Ok(self.authorize(caller, "grant.revoke", target.scope.anchor()).await?.grant),
        }
    }
}
