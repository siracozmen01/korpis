// SPDX-License-Identifier: AGPL-3.0-or-later

//! How the tests' control plane is configured to serve.

use korpis_server::data::DataDir;
use std::time::Duration;

/// How the tests' control plane serves: as `login`, on `port` of the loopback address, a panel from
/// `web_root` with its passkeys bound to `localhost` when one is given.
pub(crate) fn serve_config(
    login: String,
    data: &DataDir,
    port: u16,
    acme: korpis_server::acme::AcmeConfig,
    web_root: Option<std::path::PathBuf>,
) -> korpis_server::ServeConfig {
    let relying_party = web_root.as_ref().map(|_| korpis_core::webauthn::RelyingParty {
        id: "localhost".into(),
        origins: vec![format!("https://localhost:{port}")],
    });
    korpis_server::ServeConfig {
        data: data.clone(),
        database_url: login,
        listen: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        // The watch feed's listener holds one for good, and each node's assignments are computed
        // under its own lock, holding another while it waits: four left a request of a two-node
        // test waiting thirty seconds for one. Eight for each of the tests run at once stays
        // under the development Postgres's 300.
        max_connections: 8,
        fetch_allow: Vec::new(),
        plain_http_registries: vec![
            std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned()),
        ],
        placement_policy: korpis_core::schedule::PlacementPolicy::default(),
        placement_retry: Duration::from_millis(200),
        watch_resync: Duration::from_secs(1),
        heartbeat_interval: Duration::from_secs(30),
        // Long enough that a renewal delayed on a loaded machine does not let a lease lapse: a
        // run on a lapsed lease is judged lost (§4.2 of docs/05-scheduling.md), which is right
        // of a node gone silent and wrong of one that was merely slow. Nothing here tests
        // expiry; the agent's and the API's tests do, at terms of their own.
        lease_term: Duration::from_secs(15),
        metering_interval: std::time::Duration::from_secs(3600),
        metering_lateness: std::time::Duration::from_secs(86_400),
        denial_window: Duration::from_secs(60),
        event_retention: Duration::from_secs(30 * 86_400),
        stream_retention_floor: std::time::Duration::ZERO,
        relay_role: true,
        proxy_protocol_from: Vec::new(),
        dns_resolver: None,
        acme,
        rates: korpis_core::rate::Rates::default(),
        relying_party,
        session_limits: korpis_core::account::session::SessionLimits::default(),
        web_root,
        overlay_range: "10.48.0.0/12".parse().unwrap(),
    }
}
