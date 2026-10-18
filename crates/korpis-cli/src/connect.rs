// SPDX-License-Identifier: AGPL-3.0-or-later

//! How the CLI reaches a control plane: its URL, the installation's certificate authority, which
//! the CLI trusts alone for it (the system's roots are never consulted, so a public CA cannot
//! vouch for a Korpis control plane), and the bearer of its calls: a capability token read from
//! the environment or a file the operator names, or the access token of the session `korpis
//! login` began, renewed while the command runs (§7 of docs/13-surface-cli.md). Inside a
//! workload, the workload's own credential, carried through its node's relay ([`crate::inside`]).

use crate::error::CliError;
use crate::session::{self, Access, Held, MARGIN};
use connectrpc::client::{ClientConfig, HttpClient};
use korpis_extension::Transport;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Where and as whom.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// The control plane, e.g. `https://korpis.example:7443`.
    pub server: String,
    /// The installation CA, in PEM.
    pub ca: PathBuf,
    /// The capability token, an API token's secret, or a share link: the link's URL as it was
    /// sent, or the credential in its fragment (§6.1 of docs/08-identity.md).
    pub token: String,
}

/// The credential `text` presents: a share link's fragment when it is a link's URL, which is
/// how a link is pasted; itself otherwise.
#[must_use]
pub fn credential(text: &str) -> &str {
    match text.split_once('#') {
        Some((_, fragment)) if fragment.starts_with("korpislk_") => fragment,
        _ => text,
    }
}

/// An open way to the control plane.
#[derive(Clone)]
pub struct Connection {
    http: Transport,
    config: ClientConfig,
    server: String,
    /// A session's access token, which a task renews while the command runs.
    bearer: Option<Arc<RwLock<String>>>,
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
            let presented = credential(&e.token);
            config = config.with_default_header("authorization", format!("Bearer {presented}"));
        }
        let server = e.server.trim_end_matches('/').to_owned();
        let http = Transport::Direct(Box::new(HttpClient::with_tls(Arc::new(tls))));
        Ok(Self { http, config, server, bearer: None })
    }

    /// A connection to `server` over `http`, its calls borne by whatever `bearer` holds when
    /// each is made, which its holder keeps current.
    pub fn borne(
        http: Transport,
        server: &str,
        bearer: Arc<RwLock<String>>,
    ) -> Result<Self, CliError> {
        let uri: http::Uri = server.parse().map_err(|err| local("the server", err))?;
        let server = server.trim_end_matches('/').to_owned();
        Ok(Self { http, config: ClientConfig::new(uri), server, bearer: Some(bearer) })
    }

    /// The same connection, its calls borne by the session `held` holds: an access token now,
    /// and a task that replaces it before it expires for as long as the process runs.
    pub async fn with_session(&self, held: Held) -> Result<Self, CliError> {
        let first = session::access(self, &held).await?;
        let bearer = Arc::new(RwLock::new(first.token.clone()));
        tokio::spawn(renew(self.clone(), held, Arc::clone(&bearer), first));
        Ok(Self { bearer: Some(bearer), ..self.clone() })
    }

    /// The same trust, at `url` and anonymous: a relay, where a session is authorized by its
    /// capability token alone and the operator's token is not sent. Through a node's relay, which
    /// reaches the control plane alone, only a relay the control plane serves itself.
    pub fn at(&self, url: &str) -> Result<Self, CliError> {
        let uri: http::Uri = url.parse().map_err(|err| local("the relay", err))?;
        let own: Option<http::Uri> = self.server.parse().ok();
        let elsewhere = own.and_then(|o| o.authority().cloned()) != uri.authority().cloned();
        if matches!(self.http, Transport::Relayed(_)) && elsewhere {
            return Err(CliError::Local(format!(
                "{url} is not the control plane, and inside a workload the command reaches the \
                 control plane alone"
            )));
        }
        Ok(Self {
            http: self.http.clone(),
            config: ClientConfig::new(uri),
            server: url.to_owned(),
            bearer: None,
        })
    }

    /// The control plane's URL, without a trailing slash.
    #[must_use]
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The transport, for a generated client.
    #[must_use]
    pub fn http(&self) -> Transport {
        self.http.clone()
    }

    /// The configuration, for a generated client: with the session's current access token, if
    /// its calls are a session's.
    #[must_use]
    pub fn config(&self) -> ClientConfig {
        let token = self.bearer.as_ref().and_then(|b| b.read().ok().map(|t| t.clone()));
        match token {
            Some(t) => {
                self.config.clone().with_default_header("authorization", format!("Bearer {t}"))
            }
            None => self.config.clone(),
        }
    }
}

/// Replaces the access token in `bearer`, `current` until then, before each expires, through
/// `anonymous`, for as long as the process runs or the session lasts.
async fn renew(anonymous: Connection, held: Held, bearer: Arc<RwLock<String>>, current: Access) {
    let mut current = current;
    loop {
        tokio::time::sleep(current.usable_for().max(MARGIN)).await;
        let Ok(next) = session::access(&anonymous, &held).await else { return };
        if let Ok(mut token) = bearer.write() {
            token.clone_from(&next.token);
        }
        current = next;
    }
}
