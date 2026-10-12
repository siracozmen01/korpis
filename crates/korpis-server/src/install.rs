// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server install`: everything installation needs outside the store (the data
//! directory, the signing keys, the installation's CA) and then the install use case, which
//! creates the root organization and the first grant (§9.1 of docs/08-identity.md).

use crate::data::DataDir;
use crate::keys::{KeyError, Keys};
use crate::tls::{self, TlsError};
use korpis_app::install::Installation;
use korpis_app::ports::SystemClock;
use korpis_app::{App, AppError, NoRecipes};
use korpis_core::tenancy::Name;
use korpis_store::PgStore;

/// Why installation failed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The data directory could not be prepared.
    #[error("data directory: {0}")]
    Data(#[from] std::io::Error),
    /// The signing keys could not be loaded or created.
    #[error("keys: {0}")]
    Keys(#[from] KeyError),
    /// The certificates could not be created.
    #[error("TLS: {0}")]
    Tls(#[from] TlsError),
    /// Connecting or migrating failed.
    #[error("store: {0}")]
    Store(#[from] korpis_store::Error),
    /// The install use case refused or failed.
    #[error("{0}")]
    App(#[from] AppError),
}

/// What install needs.
#[derive(Debug, Clone)]
pub struct InstallConfig {
    /// Where keys and TLS material go.
    pub data: DataDir,
    /// Connects with schema-owner rights, which migrations need.
    pub owner_url: String,
    /// The root organization's name.
    pub organization: Name,
    /// Names and addresses the server certificate is valid for.
    pub tls_names: Vec<String>,
}

/// Installs Korpis into an empty database.
pub async fn install(config: &InstallConfig) -> Result<Installation, InstallError> {
    config.data.create()?;
    korpis_store::migrate(&config.owner_url).await?;
    let keys = Keys::load_or_create(&config.data)?;
    if !config.data.tls().join("server.pem").exists() {
        tls::generate(&config.data, &config.tls_names)?;
    }
    let store = PgStore::connect(&config.owner_url, 2).await?;
    store.ensure_partitions(3).await?;
    let app = App::new(store, SystemClock, keys.token.clone(), NoRecipes);
    app.register_keys(&keys.public()).await?;
    Ok(app.install(config.organization.clone()).await?)
}
