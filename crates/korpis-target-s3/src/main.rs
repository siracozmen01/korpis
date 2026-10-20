// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-target-s3`: the provider, run as its installation's workload. Its configuration is its
//! environment (`config::VARIABLES`), and its identity is the one the node serves the workload.

use korpis_extension::backup::Backup;
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, IdentitySocket};
use korpis_proto::korpis::v1 as pb;
use korpis_target_s3::{S3Target, config};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()
    {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "no HTTP client");
            return ExitCode::FAILURE;
        }
    };
    let env = std::env::vars().collect();
    let config = match config::from(&env, http) {
        Ok(c) => c,
        Err(e) => {
            let known: Vec<&str> = config::VARIABLES.iter().map(|(k, _)| *k).collect();
            tracing::error!(error = %e, variables = ?known, "not configured");
            return ExitCode::FAILURE;
        }
    };
    let target = match S3Target::new(config.place, config.sts) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "the place cannot be addressed");
            return ExitCode::FAILURE;
        }
    };
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_BACKUP_TARGET,
        name: config.name,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        concurrency: config.concurrency,
        sign_in: false,
    };
    let connection = Arc::new(Connection::new(IdentitySocket::default()));
    let e = Provider::new(connection, registration, Backup(target)).run().await;
    tracing::error!(error = %e, "refused for good");
    ExitCode::FAILURE
}
