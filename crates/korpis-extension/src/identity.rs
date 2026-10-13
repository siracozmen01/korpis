// SPDX-License-Identifier: Apache-2.0

//! Who the extension is, and how it reaches the control plane (§9.5 of docs/08-identity.md). A
//! credential lasts fifteen minutes and renewing it is asking the socket again, which is a fresh
//! proof that the node still runs the workload; a [`Connection`] renews it before it expires.

use crate::error::Error;
use connectrpc::client::{ClientConfig, Http2Connection, HttpClient};
use korpis_proto::korpis::agent::v1 as agent;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;

/// Where a workload's identity socket is, inside the workload.
pub const SOCKET: &str = "/run/korpis/identity.sock";

/// How long before its expiry a credential is renewed.
const RENEW_BEFORE: Duration = Duration::from_secs(300);

/// A credential, and where it is used.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential {
    /// The bearer secret.
    pub secret: String,
    /// When it stops working.
    pub expires_at: SystemTime,
    /// The control plane's address.
    pub control_plane: String,
    /// The CA its certificate is verified under, PEM.
    pub ca_pem: Vec<u8>,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("expires_at", &self.expires_at)
            .field("control_plane", &self.control_plane)
            .finish_non_exhaustive()
    }
}

/// Where credentials come from.
pub trait CredentialSource: Send + Sync + 'static {
    /// A fresh credential.
    fn fetch(&self) -> impl Future<Output = Result<Credential, Error>> + Send;
}

/// The identity socket the node serves the workload: the source inside a workload.
#[derive(Debug, Clone)]
pub struct IdentitySocket {
    path: PathBuf,
}

impl Default for IdentitySocket {
    fn default() -> Self {
        Self { path: PathBuf::from(SOCKET) }
    }
}

impl IdentitySocket {
    /// The socket at `path`, for a workload whose mount is elsewhere, or a test.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl CredentialSource for IdentitySocket {
    async fn fetch(&self) -> Result<Credential, Error> {
        let authority = http::Uri::from_static("http://localhost");
        let conn = Http2Connection::lazy_unix(&self.path, authority.clone()).shared(1);
        let client = agent::WorkloadIdentityServiceClient::new(conn, ClientConfig::new(authority));
        let answer = client.get_credential(agent::GetCredentialRequest::default()).await?;
        let answer = answer.into_owned();
        let c = answer
            .credential
            .into_option()
            .ok_or_else(|| Error::Protocol("an answer without a credential".into()))?;
        let expires_at = c.expires_at.as_option().map_or(SystemTime::UNIX_EPOCH, |t| {
            let secs = u64::try_from(t.seconds).unwrap_or(0);
            SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
        });
        Ok(Credential {
            secret: c.secret,
            expires_at,
            control_plane: answer.control_plane,
            ca_pem: answer.ca_pem,
        })
    }
}

/// A credential given rather than asked for: for tools and tests outside a workload. It is never
/// renewed.
#[derive(Debug, Clone)]
pub struct Fixed(pub Credential);

impl CredentialSource for Fixed {
    fn fetch(&self) -> impl Future<Output = Result<Credential, Error>> + Send {
        std::future::ready(Ok(self.0.clone()))
    }
}

/// The transport to the control plane for a credential's address and CA.
fn transport(c: &Credential) -> Result<HttpClient, Error> {
    let local = |what: &str, e: &dyn std::fmt::Display| Error::Local(format!("{what}: {e}"));
    let ca = CertificateDer::from_pem_slice(&c.ca_pem).map_err(|e| local("the CA", &e))?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).map_err(|e| local("the CA", &e))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| local("TLS", &e))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(HttpClient::with_tls(Arc::new(tls)))
}

/// A way to the control plane as the extension, renewing its credential as it goes.
pub struct Connection<S> {
    source: S,
    current: Mutex<Option<(Credential, HttpClient)>>,
}

impl<S> std::fmt::Debug for Connection<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

impl<S: CredentialSource> Connection<S> {
    /// A connection whose credentials come from `source`; nothing is asked until first use.
    #[must_use]
    pub fn new(source: S) -> Self {
        Self { source, current: Mutex::new(None) }
    }

    /// The transport and configuration for a generated client, with a credential that is good
    /// for at least a few minutes more.
    pub async fn client(&self) -> Result<(HttpClient, ClientConfig), Error> {
        let mut current = self.current.lock().await;
        let fresh = current.as_ref().is_some_and(|(c, _)| {
            c.expires_at.duration_since(SystemTime::now()).is_ok_and(|left| left > RENEW_BEFORE)
        });
        if !fresh {
            let c = self.source.fetch().await?;
            let reuse = current
                .as_ref()
                .filter(|(old, _)| old.control_plane == c.control_plane && old.ca_pem == c.ca_pem);
            let http = match reuse {
                Some((_, http)) => http.clone(),
                None => transport(&c)?,
            };
            *current = Some((c, http));
        }
        let (c, http) = current.as_ref().ok_or_else(|| Error::Local("no credential".into()))?;
        let uri: http::Uri =
            c.control_plane.parse().map_err(|e| Error::Local(format!("the address: {e}")))?;
        let config = ClientConfig::new(uri)
            .with_default_header("authorization", format!("Bearer {}", c.secret));
        Ok((http.clone(), config))
    }
}
