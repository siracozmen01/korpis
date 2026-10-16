// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server install`: everything installation needs outside the store (the data
//! directory, the signing keys, the installation's CA) and then the install use case, which
//! creates the root organization and the first grant (§9.1 of docs/08-identity.md); and
//! `korpis-server admin recover-root`, which issues another at the machine (§9.2).

use crate::data::DataDir;
use crate::keys::{KeyError, Keys};
use crate::tls::{self, TlsError};
pub use korpis_app::install::RootHolder;
use korpis_app::install::{Installation, RootCredential, RootInvitation};
use korpis_app::ports::SystemClock;
use korpis_app::{App, AppError, NoRecipes};
use korpis_core::id::{Id, IdKind};
use korpis_core::tenancy::Name;
use korpis_store::PgStore;
use std::fmt::Write as _;

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
    /// The directory named is not an installation's data directory.
    #[error("{0} is not an installation's data directory: it holds no server certificate")]
    NotInstalled(String),
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
    /// What holds the root grant: an account invited to register a passkey, or an API token.
    pub root: RootHolder,
}

/// Installs Korpis into an empty database.
pub async fn install(config: &InstallConfig) -> Result<Installation, InstallError> {
    config.data.create()?;
    korpis_store::migrate(&config.owner_url).await?;
    let keys = Keys::load_or_create(&config.data)?;
    if !config.data.tls().join("server.pem").exists() {
        tls::generate(&config.data, &config.tls_names)?;
    }
    let store =
        PgStore::connect(&config.owner_url, 2, std::sync::Arc::new(crate::bodies::ProtoJsonBodies))
            .await?;
    store.ensure_partitions(3).await?;
    let app = App::new(store, SystemClock, keys.token.clone(), NoRecipes);
    app.register_keys(&keys.public()).await?;
    Ok(app.install(config.organization.clone(), config.root).await?)
}

/// What `korpis-server admin recover-root` needs: the machine's data directory, whose keys the
/// control plane runs with, and its database.
#[derive(Debug, Clone)]
pub struct RecoverConfig {
    /// The installation's data directory.
    pub data: DataDir,
    /// Connects to the installation's database.
    pub database_url: String,
}

/// Issues a new root grant to a new account, invited for an hour, at the machine (§9.2 of
/// docs/08-identity.md), recorded as asked by `operator`, the operating-system user who ran it.
pub async fn recover_root(
    config: &RecoverConfig,
    operator: &str,
) -> Result<RootInvitation, InstallError> {
    if !config.data.tls().join("server.pem").exists() {
        return Err(InstallError::NotInstalled(config.data.root().display().to_string()));
    }
    let keys = Keys::load_or_create(&config.data)?;
    let bodies = std::sync::Arc::new(crate::bodies::ProtoJsonBodies);
    let store = PgStore::connect(&config.database_url, 2, bodies).await?;
    let app = App::new(store, SystemClock, keys.token.clone(), NoRecipes);
    Ok(app.recover_root(operator).await?)
}

/// What installation tells the operator: where its CA is, and the root credential, once.
#[must_use]
pub fn installed_report(config: &InstallConfig, done: &Installation) -> String {
    let org = Id::from_uuid(IdKind::Organization, done.organization.id);
    let mut out = String::from("Korpis is installed.\n\n");
    let _ = writeln!(out, "  root organization  {org}");
    let _ = writeln!(out, "  CA certificate     {}\n", config.data.tls().join("ca.pem").display());
    match &done.root {
        RootCredential::Invitation(invitation) => out.push_str(&invitation_report(invitation)),
        RootCredential::Token(_, secret) => {
            out.push_str("The root API token is shown once. Store it somewhere safe:\n\n");
            let _ = writeln!(out, "  {}\n", secret.expose());
            out.push_str(
                "It holds the root grant, and every other grant descends from it. It is a\n",
            );
            out.push_str("bearer secret with no second factor (§9.1 of docs/08-identity.md).\n");
        }
    }
    out
}

/// What an invitation for a root account tells the operator.
#[must_use]
pub fn invitation_report(invitation: &RootInvitation) -> String {
    let expires = utc(invitation.expires_at);
    let mut out = String::from("The root account's invitation is shown once:\n\n");
    let _ = writeln!(out, "  {}\n", invitation.secret.expose());
    let _ = writeln!(out, "Redeem it in the panel by registering a passkey before {expires}.");
    out.push_str("The account it creates holds the root grant; every other grant descends from\n");
    out.push_str(
        "it. Unredeemed, it expires, and `korpis-server admin recover-root` makes another.\n",
    );
    out
}

/// `t` in UTC, to the second, as RFC 3339 writes it.
fn utc(t: std::time::SystemTime) -> String {
    let t = time::OffsetDateTime::from(t);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}
