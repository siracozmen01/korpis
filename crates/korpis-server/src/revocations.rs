// SPDX-License-Identifier: AGPL-3.0-or-later

//! The revocation list this replica publishes to the sessions it holds (§6.3 of
//! docs/08-identity.md). Every replica computes it from the same rows, so no replica has to tell
//! another: each recomputes it every [`EVERY`] and publishes it when its grants changed, and every
//! session sends the current one when it opens and each new one after. A revocation therefore
//! reaches a connected verifier within about [`EVERY`] of its commit.

use crate::api::Api;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_wire::revocation::revocation_to_pb;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// How often the list is recomputed.
pub const EVERY: Duration = Duration::from_secs(2);

/// The list as last published; none until the first computation.
#[derive(Debug)]
pub struct Revocations {
    tx: watch::Sender<Option<Arc<agent::RevocationList>>>,
}

impl Default for Revocations {
    fn default() -> Self {
        Self { tx: watch::Sender::new(None) }
    }
}

impl Revocations {
    /// The published list, and every one published after it.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<agent::RevocationList>>> {
        self.tx.subscribe()
    }

    /// Publishes `list` unless it lists exactly what the published one does; whether it did.
    #[must_use = "whether the list changed"]
    pub fn publish(&self, list: agent::RevocationList) -> bool {
        self.tx.send_if_modified(|held| {
            if held.as_ref().is_some_and(|h| h.grants == list.grants) {
                return false;
            }
            *held = Some(Arc::new(list));
            true
        })
    }
}

/// Recomputes and publishes the list every [`EVERY`]. The task ends with the process.
pub(crate) async fn keep(api: Arc<Api>) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().revocation_list().await {
            Ok(list) => {
                let entries = list.list.grants.len();
                if api.revocations().publish(revocation_to_pb(&list)) {
                    tracing::debug!(entries, "revocation list published");
                }
            }
            // Verifiers keep the last list they received; the next tick tries again.
            Err(e) => tracing::warn!(error = %e, "computing the revocation list failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(version: u64, grants: &[&str]) -> agent::RevocationList {
        agent::RevocationList {
            version,
            grants: grants.iter().map(|g| (*g).to_owned()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_list_is_published_only_when_its_grants_change() {
        let r = Revocations::default();
        let mut seen = r.subscribe();
        assert!(seen.borrow_and_update().is_none());
        assert!(r.publish(list(1, &[])), "the first is always published");
        assert!(!r.publish(list(2, &[])), "a newer version of the same list is not");
        assert!(r.publish(list(3, &["a"])));
        assert!(seen.has_changed().unwrap());
        assert_eq!(seen.borrow_and_update().as_ref().map(|l| l.version), Some(3));
    }
}
