// SPDX-License-Identifier: AGPL-3.0-or-later

//! Acting as an external identity (§5.2 of docs/08-identity.md): each presentation accepted
//! once, and the tickets handed to the clients that relayed them.

use super::StoreResult;
use korpis_core::authz::binding::{IdentityBinding, ProviderRef};
use korpis_core::authz::ticket::Ticket;
use std::future::Future;
use std::time::SystemTime;

/// Writing presentations and tickets.
pub trait AssertionWrite {
    /// Accepts `presentation` from `provider` at `at`; false if it was accepted already, which
    /// is a replay.
    fn accept_presentation(
        &mut self,
        provider: &ProviderRef,
        presentation: &str,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Inserts `binding`, a person's at their first use, unless their subject was ever bound:
    /// live already, as a first use racing another may find it, or retired by an operator, whose
    /// decision a later use does not undo. Whether it was inserted.
    fn bind_at_first_use(
        &mut self,
        binding: &IdentityBinding,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Keeps a ticket, as the digest of its secret.
    fn insert_ticket(
        &mut self,
        ticket: &Ticket,
        digest: &[u8; 32],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Uses the ticket whose secret has `digest`, if it is unused, unexpired at `now`, and was
    /// handed to the client whose principal key is `relay`; it works no more after.
    fn redeem_ticket(
        &mut self,
        digest: &[u8; 32],
        relay: &str,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<Ticket>>> + Send;

    /// Forgets presentations accepted, and tickets expired, before `before`; how many.
    fn forget_assertions(
        &mut self,
        before: SystemTime,
    ) -> impl Future<Output = StoreResult<u64>> + Send;
}
