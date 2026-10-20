// SPDX-License-Identifier: AGPL-3.0-or-later

//! Acting for an asserted person (§5.2 of docs/08-identity.md): a client that relayed what a
//! person presented makes the call with its own credential and the ticket it was handed, and
//! the gateway acts as that person, and as a member of the sets the assertion named, for the
//! one action on the one object the ticket names. The ticket is spent as the call is
//! authenticated, whatever the call then does.

use crate::App;
use crate::caller::{Caller, Relayed};
use crate::error::AppError;
use crate::ports::{AssertionWrite as _, Clock, RecipeSource, Store, Tx as _};
use korpis_core::authz::credential::{Secret, SecretKind};
use korpis_core::authz::ticket::sets_of;
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::principal::Principal;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The caller a ticket presented beside `relay`'s own credential acts as: the person it
    /// asserted, for its one action on its one object, if it is unused and unexpired, was handed
    /// to `relay`, and `relay` still holds `identity.relay` where the provider is installed.
    /// Unauthenticated otherwise. The person's address is not known and no second factor is
    /// theirs, so a grant with a network or second factor condition does not permit the call.
    pub async fn relayed(&self, relay: &Caller, presented: &str) -> Result<Caller, AppError> {
        if relay.relayed.is_some() {
            return Err(AppError::Unauthenticated);
        }
        let secret =
            Secret::presented_as(SecretKind::Ticket, presented).ok_or(AppError::Unauthenticated)?;
        let (digest, by) = (secret.digest(), relay.principal.key());
        let mut tx = self.store.begin().await?;
        let redeemed = tx.redeem_ticket(&digest, &by, self.clock.now()).await?;
        let ticket = redeemed.ok_or(AppError::Unauthenticated)?;
        tx.commit().await?;
        let provider = ObjectRef::new(Kind::Organization, ticket.provider.organization);
        if !self.permitted(relay, "identity.relay", provider).await? {
            return Err(AppError::Unauthenticated);
        }
        let mut caller = Caller::new(ticket.subject.clone(), None, false);
        caller.memberships = sets_of(&ticket).iter().map(Principal::subject).collect();
        caller.relayed = Some(Relayed {
            relay: relay.principal.clone(),
            ticket: ticket.id,
            action: ticket.action,
            object: ticket.object,
        });
        Ok(caller)
    }
}
