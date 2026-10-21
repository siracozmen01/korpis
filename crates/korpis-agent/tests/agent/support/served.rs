// SPDX-License-Identifier: AGPL-3.0-or-later

//! A control plane served on a runtime of its own, so that stopping it ends every task it
//! started and closes every connection it held, as stopping the process would: what a restore
//! needs of the control plane it restores, and of the database it copies.

use korpis_server::ServeConfig;
use korpis_server::data::DataDir;
use std::net::SocketAddr;
use std::time::Duration;

/// How the harness serves: as installation does, by a login that owns nothing.
pub fn config(data: &DataDir, login: &str, listen: SocketAddr, relay_role: bool) -> ServeConfig {
    ServeConfig {
        data: data.clone(),
        database_url: login.to_owned(),
        listen,
        max_connections: 4,
        fetch_allow: Vec::new(),
        plain_http_registries: vec![
            std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned()),
        ],
        placement_policy: korpis_core::schedule::PlacementPolicy::default(),
        placement_retry: Duration::from_millis(200),
        watch_resync: Duration::from_secs(1),
        heartbeat_interval: Duration::from_secs(30),
        lease_term: Duration::from_secs(2),
        metering_interval: std::time::Duration::from_secs(3600),
        metering_lateness: std::time::Duration::from_secs(86_400),
        denial_window: Duration::from_secs(60),
        event_retention: Duration::from_secs(30 * 86_400),
        stream_retention_floor: Duration::ZERO,
        relay_role,
        proxy_protocol_from: Vec::new(),
        dns_resolver: None,
        acme: korpis_server::acme::AcmeConfig::default(),
        rates: korpis_core::rate::Rates::default(),
        relying_party: None,
        session_limits: korpis_core::account::session::SessionLimits::default(),
        web_root: None,
    }
}

/// A control plane serving on its own runtime, until it is stopped or dropped.
pub struct Served {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Where it listens.
    pub addr: SocketAddr,
}

impl Served {
    /// Serves `config` on a runtime of its own; returns once it listens.
    pub async fn start(config: ServeConfig) -> Self {
        let (listening, addr) = tokio::sync::oneshot::channel();
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime =
                tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
            runtime.block_on(async move {
                let running = korpis_server::serve(config).await.expect("serve");
                let _ = listening.send(running.addr);
                let _ = tokio::task::spawn_blocking(move || stopped.recv()).await;
            });
            runtime.shutdown_timeout(Duration::from_secs(10));
        });
        let addr = addr.await.expect("the control plane listens");
        Self { stop: Some(stop), thread: Some(thread), addr }
    }

    /// Stops it: every task it started ends, and every connection it held closes.
    pub fn stop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.stop();
    }
}
