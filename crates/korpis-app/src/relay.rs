// SPDX-License-Identifier: AGPL-3.0-or-later

//! Relays that run as their own process (§3.1 of docs/02-architecture.md). A relay carries every
//! organization's sessions, so it is enrolled, listed, and removed with authority over the root
//! organization: `node.enroll` to enroll or remove one, `node.read` to list them. The control
//! plane's own relay role is not a relay here; it needs no enrollment.

mod enroll;
mod feed;
mod join;
mod renew;

pub use enroll::{CreateRelayEnrollment, RelayEnrollmentToken};
pub use feed::RelayFeed;
pub use join::{JoinRelay, JoinedRelay};

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite as _, RecipeSource, RelayWrite as _, Store};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::relay::Relay;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// `organization`, if it is the root: relays are the whole installation's.
    pub(crate) async fn root(&self, organization: Uuid) -> Result<ObjectRef, AppError> {
        let org = self
            .store
            .organization(organization)
            .await?
            .ok_or(AppError::NotFound { kind: "organization" })?;
        if org.parent.is_some() {
            return Err(AppError::invalid(
                "organization",
                "relays carry every organization's sessions; they belong to the root",
            ));
        }
        Ok(ObjectRef::new(Kind::Organization, organization))
    }

    /// Every relay, removed ones included: `node.read` over the root organization.
    pub async fn list_relays(
        &self,
        caller: &Caller,
        organization: Uuid,
    ) -> Result<Vec<Relay>, AppError> {
        let root = self.root(organization).await?;
        self.authorize(caller, "node.read", root).await?;
        Ok(self.store.relays().await?)
    }

    /// Removes a relay: `node.enroll` over the root organization. It is named to no node and told
    /// nothing more; the certificate it holds stays valid until it expires, since nothing here
    /// revokes certificates, and nodes stop attaching to it once they are told.
    pub async fn remove_relay(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Relay>,
        (organization, id): (Uuid, Uuid),
    ) -> Result<Reply<Relay>, AppError> {
        const OPERATION: &str = "relay.remove";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let root = self.root(organization).await?;
        let permit = self.authorize(caller, "node.enroll", root).await?;
        let mut relay = self.store.relay(id).await?.ok_or(AppError::NotFound { kind: "relay" })?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        if !tx.remove_relay(id, now).await? {
            return Err(AppError::conflict("relay.removed", "the relay is removed already"));
        }
        relay.removed_at = Some(now);
        let effect = Effect::new(caller.principal.clone(), OPERATION, Outcome::Succeeded)
            .under(permit.grant)
            .after(details([
                ("relay", id.to_string().into()),
                ("name", relay.name.as_str().into()),
            ]));
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, OPERATION, relay).await
    }
}
