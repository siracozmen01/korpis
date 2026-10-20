// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-target-fs`: the provider, run as its installation's workload, serving its own volume.
//! Its configuration is its environment (`config::VARIABLES`), and its identity is the one the
//! node serves the workload.

use korpis_extension::backup::Backup;
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, IdentitySocket};
use korpis_proto::korpis::v1 as pb;
use korpis_target_fs::serve::{Served, serve};
use korpis_target_fs::store::Store;
use korpis_target_fs::tokens::Tokens;
use korpis_target_fs::{FsTarget, config, tls};
use std::process::ExitCode;
use std::sync::Arc;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let config = match config::from(&env) {
        Ok(c) => c,
        Err(e) => {
            let known: Vec<&str> = config::VARIABLES.iter().map(|(k, _)| *k).collect();
            tracing::error!(error = %e, variables = ?known, "not configured");
            return ExitCode::FAILURE;
        }
    };
    let (store, identity) = match (Store::open(&config.root), tls::identity()) {
        (Ok(s), Ok(i)) => (s, i),
        (Err(e), _) => {
            tracing::error!(error = %e, root = %config.root.display(), "no store");
            return ExitCode::FAILURE;
        }
        (_, Err(e)) => {
            tracing::error!(error = %e, "no certificate");
            return ExitCode::FAILURE;
        }
    };
    let listener = match tokio::net::TcpListener::bind(config.listen).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, listen = %config.listen, "cannot listen");
            return ExitCode::FAILURE;
        }
    };
    let tokens = Arc::new(Tokens::default());
    let served = Served { store: store.clone(), tokens: tokens.clone() };
    tokio::spawn(serve(listener, identity.config, served));
    let target = FsTarget::new(store, tokens, config.endpoint, identity.certificate);
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_BACKUP_TARGET,
        name: config.name,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        concurrency: 8,
        sign_in: false,
    };
    let connection = Arc::new(Connection::new(IdentitySocket::default()));
    let e = Provider::new(connection, registration, Backup(target)).run().await;
    tracing::error!(error = %e, "refused for good");
    ExitCode::FAILURE
}
