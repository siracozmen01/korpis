// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider streams this replica holds (§3.3 of docs/16-extensions.md), by registration,
//! with the calls sent down each and not yet answered. A registration withdrawn elsewhere (by an
//! uninstall, or by expiry) has its stream closed here when the replica next confirms what it
//! holds.

use korpis_core::extension::Registration;
use korpis_proto::korpis::v1::ProviderControl;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

struct Held {
    registration: Registration,
    tx: Sender<ProviderControl>,
    /// Calls sent down the stream and not yet answered.
    in_flight: HashSet<Uuid>,
}

/// The registry.
pub struct Providers {
    replica: Uuid,
    open: Mutex<HashMap<Uuid, Held>>,
}

impl std::fmt::Debug for Providers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Providers")
            .field("replica", &self.replica)
            .field("open", &self.guard().len())
            .finish()
    }
}

impl Default for Providers {
    fn default() -> Self {
        Self { replica: Uuid::now_v7(), open: Mutex::new(HashMap::new()) }
    }
}

impl Providers {
    // Every operation changes one entry with no await inside, so a poisoned map is consistent.
    fn guard(&self) -> MutexGuard<'_, HashMap<Uuid, Held>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// This replica's identity, which its registrations name.
    #[must_use]
    pub const fn replica(&self) -> Uuid {
        self.replica
    }

    /// Holds a registration's stream, sending through `tx`.
    pub fn hold(&self, registration: Registration, tx: Sender<ProviderControl>) {
        let held = Held { registration, tx, in_flight: HashSet::new() };
        self.guard().insert(held.registration.id, held);
    }

    /// Lets go of a registration's stream; the registration, if it was held.
    pub fn release(&self, id: Uuid) -> Option<Registration> {
        self.guard().remove(&id).map(|h| h.registration)
    }

    /// Closes every stream of `before` whose registration is not in `alive`: dropping its sender
    /// ends the stream the provider reads. `before` is what was held when `alive` was asked
    /// for, and only those streams are judged by it: a registration commits before its stream
    /// is held, so one held then was committed before the question and is in `alive` unless
    /// it was withdrawn, while one held since may have committed after the question was
    /// answered, and is judged at the next confirmation instead of being closed for a race.
    pub fn close_withdrawn(&self, before: &[Uuid], alive: &[Uuid]) {
        self.guard().retain(|id, _| !before.contains(id) || alive.contains(id));
    }

    /// The registrations whose streams this replica holds now.
    #[must_use]
    pub fn held_ids(&self) -> Vec<Uuid> {
        self.guard().keys().copied().collect()
    }

    /// Every registration whose stream this replica holds.
    #[must_use]
    pub fn held(&self) -> Vec<Registration> {
        self.guard().values().map(|h| h.registration.clone()).collect()
    }

    /// Sends `message` down a held stream as the call `call`, which is then in flight; `false`
    /// if the stream is gone or full, and the call was not sent.
    #[must_use]
    pub fn send_call(&self, id: Uuid, call: Uuid, message: ProviderControl) -> bool {
        let mut open = self.guard();
        let Some(held) = open.get_mut(&id) else { return false };
        if held.tx.try_send(message).is_err() {
            return false;
        }
        held.in_flight.insert(call);
        true
    }

    /// Takes `call` out of flight on a held stream; whether it was in flight.
    pub fn settle(&self, id: Uuid, call: Uuid) -> bool {
        self.guard().get_mut(&id).is_some_and(|h| h.in_flight.remove(&call))
    }

    /// Sends `message` down a held stream if `call` is still in flight on it, taking it out of
    /// flight: how a call is cancelled at its deadline. Nothing if it was answered meanwhile.
    pub fn cancel(&self, id: Uuid, call: Uuid, message: ProviderControl) {
        let mut open = self.guard();
        if let Some(held) = open.get_mut(&id)
            && held.in_flight.remove(&call)
        {
            // A full stream drops the cancellation: the call has finished here regardless.
            let _ = held.tx.try_send(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_core::extension::ProviderType;
    use std::time::SystemTime;

    fn held(p: &Providers) -> (Uuid, tokio::sync::mpsc::Receiver<ProviderControl>) {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let id = Uuid::now_v7();
        let registration = Registration {
            id,
            extension: Uuid::now_v7(),
            organization: Uuid::now_v7(),
            kind: ProviderType::Dns,
            name: "dns".into(),
            version: "1.0.0".into(),
            concurrency: 1,
            sign_in: false,
            fencing: korpis_core::node::driver::Fencing::None,
            replica: p.replica(),
            connected_at: SystemTime::now(),
            seen_at: SystemTime::now(),
        };
        p.hold(registration, tx);
        (id, rx)
    }

    #[test]
    fn only_a_stream_held_before_the_confirmation_is_closed_for_missing_from_it() {
        let p = Providers::default();
        let (withdrawn, _a) = held(&p);
        let (confirmed, _b) = held(&p);
        let before = p.held_ids();
        // Registered after the confirmation read the database: missing from its answer, not
        // withdrawn.
        let (late, _c) = held(&p);
        p.close_withdrawn(&before, &[confirmed]);
        let mut left = p.held_ids();
        left.sort();
        let mut want = vec![confirmed, late];
        want.sort();
        assert_eq!(left, want, "{withdrawn} closed, the rest kept");
    }
}
