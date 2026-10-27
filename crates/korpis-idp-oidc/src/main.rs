// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-idp-oidc`: the provider, run as its installation's workload. Its configuration is its
//! environment (`config::VARIABLES`), and its identity is the one the node serves the workload.

use korpis_extension::identity_provider::Identity;
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, IdentitySocket};
use korpis_idp_oidc::{Oidc, config};
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
    // A person is waiting on every sign-in, and the control plane stops waiting at ten seconds.
    let http = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "no HTTP client");
            return ExitCode::FAILURE;
        }
    };
    let env = std::env::vars().collect();
    let config = match config::from(&env) {
        Ok(c) => c,
        Err(e) => {
            let known: Vec<&str> = config::VARIABLES.iter().map(|(k, _)| *k).collect();
            tracing::error!(error = %e, variables = ?known, "not configured");
            return ExitCode::FAILURE;
        }
    };
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_IDENTITY,
        name: config.name,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        concurrency: config.concurrency,
        sign_in: true,
        fencing: pb::Fencing::FENCING_UNSPECIFIED,
    };
    let provider = Identity(Oidc::new(http, config.settings));
    let connection = Arc::new(Connection::new(IdentitySocket::default()));
    let e = Provider::new(connection, registration, provider).run().await;
    tracing::error!(error = %e, "refused for good");
    ExitCode::FAILURE
}
