// SPDX-License-Identifier: AGPL-3.0-or-later

//! A relay running as its own process (§3.1 of docs/02-architecture.md), joined to the control
//! plane and serving in this test's process, holding nothing of the control plane's but what
//! its feed sends.

use super::{ControlPlane, mutation};
use connectrpc::client::{ClientConfig, HttpClient};
use korpis_proto::korpis::v1 as pb;
use korpis_server::standalone::{Edge, RelayDir, RunningRelay, join, run};

/// A joined, running relay; it stops when dropped.
pub struct Relay {
    /// Where clients and nodes reach it.
    pub url: String,
    running: RunningRelay,
    _dir: tempfile::TempDir,
}

impl Relay {
    /// Serves SFTP on `listener` with `host_key` as well, as `--sftp-listen` has it.
    pub fn serve_sftp(
        &self,
        listener: tokio::net::TcpListener,
        host_key: korpis_server::sftp::host_key::PrivateKey,
    ) -> tokio::task::JoinHandle<std::io::Result<()>> {
        self.running.serve_sftp(listener, host_key, None).unwrap()
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.running.task.abort();
    }
}

impl ControlPlane {
    /// A relay enrolled as `name`, joined, and running.
    pub async fn standalone_relay(&self, name: &str) -> Relay {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://localhost:{}", listener.local_addr().unwrap().port());
        let created = self
            .nodes()
            .create_relay_enrollment_token(pb::CreateRelayEnrollmentTokenRequest {
                mutation: mutation(&format!("relay-{name}")).into(),
                organization: self.root_org.clone(),
                name: name.to_owned(),
                url: url.clone(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned();
        let dir = tempfile::tempdir().unwrap();
        let relay_dir = RelayDir::new(dir.path().join("relay"));
        join(&relay_dir, &self.url, &created.token, &created.ca_fingerprint).await.unwrap();
        let running = run(relay_dir, listener, Edge::default()).await.unwrap();
        Relay { url, running, _dir: dir }
    }

    /// The relay service at `url`, as an anonymous client.
    pub fn relay_at(&self, url: &str) -> pb::RelayServiceClient<HttpClient> {
        pb::RelayServiceClient::new(self.http(), ClientConfig::new(url.parse().unwrap()))
    }
}
