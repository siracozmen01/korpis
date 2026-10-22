// SPDX-License-Identifier: AGPL-3.0-or-later

//! The SFTP servers `ServerInfo` names to clients (§3.1 of docs/02-architecture.md): the control
//! plane's own relay role's, once it serves one, and each relay's, as the replica last listed
//! them from what the relays reported in their feeds, so answering a client reads no database.

use korpis_app::relay::SftpEndpoint;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use std::sync::{OnceLock, RwLock};

/// What is advertised.
#[derive(Debug, Default)]
pub struct Advertised {
    own: OnceLock<SftpEndpoint>,
    relays: RwLock<Vec<SftpEndpoint>>,
}

fn server(e: &SftpEndpoint) -> pb::SftpServer {
    pb::SftpServer {
        address: e.address.to_string(),
        known_hosts_name: e.address.known_hosts_name(),
        host_key: e.host_key.openssh(),
        fingerprint: e.host_key.fingerprint(),
        relay: e.relay.map(|r| korpis_wire::wire_id(IdKind::Relay, r)).unwrap_or_default(),
        ..Default::default()
    }
}

impl Advertised {
    /// Advertises the control plane's own SFTP, once; whether it was not advertised already.
    pub(crate) fn serve_own(&self, endpoint: SftpEndpoint) -> bool {
        self.own.set(endpoint).is_ok()
    }

    /// Replaces the relays' with `endpoints`.
    pub(crate) fn relays_are(&self, endpoints: Vec<SftpEndpoint>) {
        if let Ok(mut held) = self.relays.write() {
            *held = endpoints;
        }
    }

    /// Every SFTP server advertised, the control plane's own first.
    #[must_use]
    pub fn listed(&self) -> Vec<pb::SftpServer> {
        let relays = self.relays.read().map(|r| r.iter().map(server).collect()).unwrap_or_default();
        self.own.get().map(server).into_iter().chain::<Vec<_>>(relays).collect()
    }
}
