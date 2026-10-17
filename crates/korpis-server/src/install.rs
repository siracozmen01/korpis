// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server install`: everything installation needs outside the store (the data
//! directory, the signing keys, the installation's CA) and then the install use case, which
//! creates the root organization and the first grant (§9.1 of docs/08-identity.md); and
//! `korpis-server admin recover-root`, which issues another at the machine (§9.2). Before any
//! of it, the command runs the preflight of §2 of docs/18-operations.md; after it, the control
//! plane is given a login of its own, its backup target is declared, and the systemd units are
//! written.

pub mod preflight;
pub mod system;
pub mod target;
pub mod units;

use crate::data::DataDir;
use crate::keys::{KeyError, Keys};
use crate::tls::{self, TlsError};
pub use korpis_app::install::RootHolder;
use korpis_app::install::{Installation, RootCredential, RootInvitation};
use korpis_app::node::EnrollmentToken;
use korpis_app::ports::SystemClock;
use korpis_app::{App, AppError, NoRecipes};
use korpis_core::id::{Id, IdKind};
use korpis_core::tenancy::Name;
use korpis_store::PgStore;
use std::fmt::Write as _;
use target::BackupTarget;

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
    /// The first node's name, whose enrollment installation makes; none makes none.
    pub first_node: Option<Name>,
    /// Where the control plane's own backup goes, outside Korpis; none declares none.
    pub backup_target: Option<BackupTarget>,
    /// The control plane's login, made by the operator; none has installation make one.
    pub database_url: Option<String>,
}

/// What installation made, besides the installation itself, to say once.
#[derive(Debug)]
pub struct Installed {
    /// The root organization, the root credential, and the first node's enrollment.
    pub installation: Installation,
    /// The URL the control plane connects with.
    pub database_url: String,
    /// The key the control plane's backup is sealed with, when it was made now.
    pub backup_key: Option<String>,
    /// The fingerprint of the installation's CA, which a node checks when it joins.
    pub ca_fingerprint: String,
}

/// The control plane's own login.
const SERVER_ROLE: &str = "korpis_server";

/// Installs Korpis into an empty database.
pub async fn install(config: &InstallConfig) -> Result<Installed, InstallError> {
    let installation = install_store(config).await?;
    let database_url = match &config.database_url {
        Some(url) => url.clone(),
        None => server_login(&config.owner_url).await?,
    };
    let backup_key = match &config.backup_target {
        Some(t) => target::declare(&config.data, t)?,
        None => None,
    };
    let ca_fingerprint = tls::ca_fingerprint(&config.data)?;
    Ok(Installed { installation, database_url, backup_key, ca_fingerprint })
}

/// Makes the control plane's own login with a new password; the URL it connects with, the
/// owner's with the login and password in place of the owner's.
async fn server_login(owner_url: &str) -> Result<String, InstallError> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let password = bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    korpis_store::server::ensure_login(owner_url, SERVER_ROLE, &password).await?;
    let mut url = url::Url::parse(owner_url)
        .map_err(|e| std::io::Error::other(format!("the owner URL: {e}")))?;
    let set = url.set_username(SERVER_ROLE).is_ok() && url.set_password(Some(&password)).is_ok();
    if !set {
        return Err(std::io::Error::other("the owner URL names no server to log in to").into());
    }
    Ok(url.to_string())
}

/// The data directory, the keys, the CA, the schema, and the installation in the store.
async fn install_store(config: &InstallConfig) -> Result<Installation, InstallError> {
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
    Ok(app.install(config.organization.clone(), config.root, config.first_node.clone()).await?)
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

/// What installation tells the operator, once: where its CA is, the root credential, the backup
/// key when it was made now, and the command that joins the first node to `server`.
#[must_use]
pub fn installed_report(config: &InstallConfig, done: &Installed, server: &str) -> String {
    let installation = &done.installation;
    let org = Id::from_uuid(IdKind::Organization, installation.organization.id);
    let mut out = String::from("Korpis is installed.\n\n");
    let _ = writeln!(out, "  root organization  {org}");
    let _ = writeln!(out, "  CA certificate     {}\n", config.data.tls().join("ca.pem").display());
    match &installation.root {
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
    if let Some(key) = &done.backup_key {
        out.push_str("\nThe key the control plane's own backup is sealed with is shown once:\n\n");
        let _ = writeln!(out, "  {key}\n");
        let _ = writeln!(
            out,
            "Keep a copy away from this machine: a restore needs it, and the copy in"
        );
        let _ = writeln!(out, "{} is lost with the machine.", config.data.backup().display());
    }
    if let Some(first) = &installation.first_node {
        out.push_str(&join_report(first, server, &done.ca_fingerprint));
    }
    out
}

/// The command that joins the first node, once the units run.
fn join_report(first: &EnrollmentToken, server: &str, fingerprint: &str) -> String {
    let name = first.enrollment.node_name.as_str();
    let mut out = String::from("\nJoin the first node, as root on it, as its agent's user:\n\n");
    let _ = writeln!(
        out,
        "  runuser -u korpis-agent -- korpis-agent join --server {server} \\\n      --token {} \
         --ca-fingerprint {fingerprint} --hostname {name}\n",
        first.secret.expose()
    );
    let _ = writeln!(
        out,
        "The token enrolls one node named {name}, until {}.",
        utc(first.enrollment.expires_at)
    );
    out
}

/// What an invitation for a root account tells the operator.
#[must_use]
pub fn invitation_report(invitation: &RootInvitation) -> String {
    let expires = utc(invitation.expires_at);
    let mut out = String::from("The root account's invitation is shown once:\n\n");
    let _ = writeln!(out, "  {}\n", invitation.secret.expose());
    let _ = writeln!(out, "Redeem it at the panel's /invitation page, pasted there, or opened as");
    let _ = writeln!(out, "/invitation#<invitation>, by registering a passkey before {expires}.");
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
