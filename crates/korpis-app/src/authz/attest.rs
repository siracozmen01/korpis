// SPDX-License-Identifier: AGPL-3.0-or-later

//! Attesting what a node reports a subject did (§6 of docs/14-streams.md). A node that carried a
//! session reports what was done on it, console input or a file session's opening and end (§3.2
//! of docs/06-storage.md), as the act of the session's subject, under the grant the session's
//! token was minted from. The control plane does not take the node's word for whose act it was:
//! it believes the report only if that grant is the subject's and, as issued, permitted the
//! action on the object at the moment the node says it happened. So a node can attribute input
//! only to someone who could have given it, never to anyone at all.
//!
//! What it leaves to others: revocation and suspension, which the node judged against the
//! revocation list it held when it carried the input (§6.3 of docs/08-identity.md); the
//! client's network, which the relay judged (§3.1 of docs/02-architecture.md); and a second
//! factor, which the gateway judged when it minted the token.

use super::Snapshot;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{self, Action, Decision, ObjectRef, Request};
use korpis_core::principal::Principal;
use std::time::SystemTime;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Whether `actor`'s grant `leaf`, as issued, permitted `action` on `object` at `at`.
    pub(crate) async fn attest(
        &self,
        actor: &Principal,
        action: &Action,
        (object, leaf): (ObjectRef, Uuid),
        at: SystemTime,
    ) -> Result<bool, AppError> {
        let caller = Caller::new(actor.clone(), None, true);
        let mut snapshot = Snapshot::default();
        snapshot.load(&self.store, object).await?;
        let mut chains = self.held_chains(&caller, Some(leaf), &mut snapshot).await?;
        for g in chains.iter_mut().flatten() {
            g.revoked = false;
            g.suspended = false;
            g.conditions.source_cidr.clear();
            g.conditions.requires_mfa = false;
        }
        let Some(resource) = authz::Hierarchy::resource(&snapshot, object) else {
            return Ok(false);
        };
        let request = Request {
            subject: &caller.subject,
            action,
            resource: &resource,
            source_ip: None,
            mfa: true,
            now: at,
            renewal: false,
        };
        let decision = authz::evaluate(&request, &chains, &snapshot);
        Ok(matches!(decision, Decision::Permit(p) if !p.redeem))
    }
}
