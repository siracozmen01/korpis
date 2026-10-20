// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-kms-openbao`: the provider, run as its installation's workload. Its configuration is
//! its environment (`config::VARIABLES`), and its identity is the one the node serves the
//! workload.

use korpis_extension::keys::Keys;
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, IdentitySocket};
use korpis_kms_openbao::{Transit, config};
use korpis_proto::korpis::v1 as pb;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let env = std::env::vars().collect();
    let config = match config::from(&env) {
        Ok(c) => c,
        Err(e) => {
            let known: Vec<&str> = config::VARIABLES.iter().map(|(k, _)| *k).collect();
            tracing::error!(error = %e, variables = ?known, "not configured");
            return ExitCode::FAILURE;
        }
    };
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build();
    let transit = http.map_err(|e| e.to_string()).and_then(|http| {
        Transit::new(&config.address, (&config.mount, config.namespace.clone()), config.token, http)
    });
    let transit = match transit {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "the engine cannot be addressed");
            return ExitCode::FAILURE;
        }
    };
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_KEY_MANAGEMENT,
        name: config.name,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        concurrency: config.concurrency,
        sign_in: false,
    };
    let connection = Arc::new(Connection::new(IdentitySocket::default()));
    let e = Provider::new(connection, registration, Keys(transit)).run().await;
    tracing::error!(error = %e, "refused for good");
    ExitCode::FAILURE
}
