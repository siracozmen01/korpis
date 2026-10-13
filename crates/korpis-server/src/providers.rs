// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider streams this replica holds (§3.3 of docs/16-extensions.md), by registration.
//! Calls for a provider are routed to the streams of the replica holding them; a registration
//! withdrawn elsewhere (by an uninstall, or by expiry) has its stream closed here when the
//! replica next confirms what it holds.

use korpis_core::extension::Registration;
use korpis_proto::korpis::v1::ProviderControl;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

struct Held {
    registration: Registration,
    tx: Sender<ProviderControl>,
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
    // Every operation inserts or removes one entry whole, so a poisoned map is consistent.
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
        self.guard().insert(registration.id, Held { registration, tx });
    }

    /// Lets go of a registration's stream; the registration, if it was held.
    pub fn release(&self, id: Uuid) -> Option<Registration> {
        self.guard().remove(&id).map(|h| h.registration)
    }

    /// Closes every held stream whose registration is not in `alive`: dropping its sender ends
    /// the stream the provider reads.
    pub fn keep_only(&self, alive: &[Uuid]) {
        self.guard().retain(|id, _| alive.contains(id));
    }

    /// The sender of a held registration's stream.
    #[must_use]
    pub fn sender(&self, id: Uuid) -> Option<Sender<ProviderControl>> {
        self.guard().get(&id).map(|h| h.tx.clone())
    }
}
