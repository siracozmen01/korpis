// SPDX-License-Identifier: AGPL-3.0-or-later

//! How the CLI reaches a control plane: its URL, the installation's certificate authority, which
//! the CLI trusts alone for it (the system's roots are never consulted, so a public CA cannot
//! vouch for a Korpis control plane), and a capability token read from the environment or a file
//! the operator names, never from a dotfile the CLI keeps (§7 of docs/13-surface-cli.md).

use crate::error::CliError;
use connectrpc::client::{ClientConfig, HttpClient};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Where and as whom.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// The control plane, e.g. `https://korpis.example:7443`.
    pub server: String,
    /// The installation CA, in PEM.
    pub ca: PathBuf,
    /// The capability token.
    pub token: String,
}

/// An open way to the control plane.
#[derive(Clone)]
pub struct Connection {
    http: HttpClient,
    config: ClientConfig,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

fn local(what: &str, e: impl std::fmt::Display) -> CliError {
    CliError::Local(format!("{what}: {e}"))
}

/// The token in `path`: its first line.
pub fn token_from(path: &Path) -> Result<String, CliError> {
    let text = std::fs::read_to_string(path).map_err(|e| local(&path.display().to_string(), e))?;
    Ok(text.lines().next().unwrap_or_default().trim().to_owned())
}

impl Connection {
    /// A connection to `e`; nothing is sent until the first call.
    pub fn open(e: &Endpoint) -> Result<Self, CliError> {
        let ca = CertificateDer::from_pem_file(&e.ca).map_err(|err| local("the CA", err))?;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca).map_err(|err| local("the CA", err))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|err| local("TLS", err))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let uri: http::Uri = e.server.parse().map_err(|err| local("the server", err))?;
        let mut config = ClientConfig::new(uri);
        if !e.token.is_empty() {
            config = config.with_default_header("authorization", format!("Bearer {}", e.token));
        }
        Ok(Self { http: HttpClient::with_tls(Arc::new(tls)), config })
    }

    /// The transport, for a generated client.
    #[must_use]
    pub fn http(&self) -> HttpClient {
        self.http.clone()
    }

    /// The configuration, for a generated client.
    #[must_use]
    pub fn config(&self) -> ClientConfig {
        self.config.clone()
    }
}
