// SPDX-License-Identifier: Apache-2.0

//! Who the extension is, and how it reaches the control plane (§9.5 of docs/08-identity.md). A
//! credential lasts fifteen minutes and renewing it is asking the socket again, which is a fresh
//! proof that the node still runs the workload; a [`Connection`] renews it before it expires.

use crate::error::Error;
use crate::transport::{Transport, transport};
use connectrpc::client::{ClientConfig, Http2Connection};
use korpis_proto::korpis::agent::v1 as agent;
use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;

/// Where a workload's identity socket is, inside the workload.
pub const SOCKET: &str = "/run/korpis/identity.sock";

/// The relay to the control plane, beside the socket, for an installed extension's workload.
pub const RELAY: &str = "api.sock";

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
    /// The relay its node serves it to the control plane, where it runs as an installed
    /// extension's workload; the network otherwise.
    pub relay: Option<PathBuf>,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("expires_at", &self.expires_at)
            .field("control_plane", &self.control_plane)
            .field("relay", &self.relay)
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

impl IdentitySocket {
    /// The relay beside the socket, if the node serves one.
    fn relay(&self) -> Option<PathBuf> {
        let relay = self.path.parent()?.join(RELAY);
        relay.exists().then_some(relay)
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
            relay: self.relay(),
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

/// A way to the control plane as the extension, renewing its credential as it goes.
pub struct Connection<S> {
    source: S,
    current: Mutex<Option<(Credential, Transport)>>,
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
    pub async fn client(&self) -> Result<(Transport, ClientConfig), Error> {
        let mut current = self.current.lock().await;
        let fresh = current.as_ref().is_some_and(|(c, _)| {
            c.expires_at.duration_since(SystemTime::now()).is_ok_and(|left| left > RENEW_BEFORE)
        });
        if !fresh {
            let c = self.source.fetch().await?;
            let reuse = current.as_ref().filter(|(old, _)| {
                (&old.control_plane, &old.ca_pem, &old.relay)
                    == (&c.control_plane, &c.ca_pem, &c.relay)
            });
            let http = match reuse {
                Some((_, http)) => http.clone(),
                None => transport(&c.control_plane, &c.ca_pem, c.relay.as_deref())?,
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
