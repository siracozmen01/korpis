// SPDX-License-Identifier: AGPL-3.0-or-later

//! The relays this replica names to the nodes it holds sessions with (§3.1 of
//! docs/02-architecture.md): every relay enrolled to run as its own process and not removed, and
//! whether this process serves the role itself. Each replica recomputes them every [`EVERY`] from
//! the same rows and publishes them when they changed; every session sends the current ones
//! when it opens and each change after, so a relay enrolled or removed reaches every connected
//! node within about [`EVERY`]. The SFTP the relays report serving is listed alongside, for
//! `ServerInfo` to name to clients.

use crate::api::Api;
use korpis_proto::korpis::agent::v1 as agent;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// How often the relays are recomputed.
pub const EVERY: Duration = Duration::from_secs(2);

/// The relays as last published; none until the first computation.
#[derive(Debug)]
pub struct NamedRelays {
    tx: watch::Sender<Option<Arc<agent::Relays>>>,
}

impl Default for NamedRelays {
    fn default() -> Self {
        Self { tx: watch::Sender::new(None) }
    }
}

impl NamedRelays {
    /// The relays published, and every change after.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<agent::Relays>>> {
        self.tx.subscribe()
    }

    /// Publishes `relays` unless they are what is published; whether it did.
    #[must_use = "whether the relays changed"]
    pub fn publish(&self, relays: agent::Relays) -> bool {
        self.tx.send_if_modified(|held| {
            if held.as_deref() == Some(&relays) {
                return false;
            }
            *held = Some(Arc::new(relays));
            true
        })
    }
}

/// Recomputes and publishes the relays every [`EVERY`]. The task ends with the process.
pub(crate) async fn keep(api: Arc<Api>) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().relay_urls().await {
            Ok(urls) => {
                let control_plane = api.serves_relay();
                let relays = agent::Relays { urls, control_plane, ..Default::default() };
                let count = relays.urls.len();
                if api.named_relays().publish(relays) {
                    tracing::info!(relays = count, control_plane, "relays named to nodes");
                }
            }
            // Nodes keep the relays they were named; the next tick tries again.
            Err(e) => tracing::warn!(error = %e, "listing the relays failed"),
        }
        match api.app().sftp_endpoints().await {
            Ok(endpoints) => api.sftp_advertised().relays_are(endpoints),
            // Clients are told of what was listed last; the next tick tries again.
            Err(e) => tracing::warn!(error = %e, "listing the relays' SFTP failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relays_are_published_only_when_they_change() {
        let named = NamedRelays::default();
        let relays = |urls: &[&str]| agent::Relays {
            urls: urls.iter().map(|u| (*u).to_owned()).collect(),
            control_plane: true,
            ..Default::default()
        };
        assert!(named.publish(relays(&[])), "the first is always published");
        assert!(!named.publish(relays(&[])));
        assert!(named.publish(relays(&["https://r1.example"])));
    }
}
