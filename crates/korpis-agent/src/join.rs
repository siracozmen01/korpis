// SPDX-License-Identifier: AGPL-3.0-or-later

//! Joining a control plane (§3 of docs/18-operations.md): the agent generates its identity key,
//! sends the public half with the one-time token over a connection verified against the pinned
//! CA, and keeps what it is told.

use crate::state::{Identity, StateDir, StateError};
use crate::tls::{self, TlsError};
use connectrpc::client::{ClientConfig, HttpClient};
use korpis_proto::korpis::agent::v1 as agent;
use std::sync::Arc;

/// Why the agent could not join.
#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    /// The state directory.
    #[error(transparent)]
    State(#[from] StateError),
    /// TLS.
    #[error(transparent)]
    Tls(#[from] TlsError),
    /// The server address is not a URL.
    #[error("server: {0}")]
    Server(String),
    /// The control plane refused or could not be reached.
    #[error("enrollment: {0}")]
    Enroll(#[from] connectrpc::ConnectError),
    /// The handshake verified no CA; it cannot happen after a successful call.
    #[error("the pinned CA was not seen")]
    NoCa,
}

/// What a join is given.
#[derive(Debug, Clone)]
pub struct Join {
    /// The control plane, e.g. `https://korpis.example:7443`.
    pub server: String,
    /// The one-time enrollment token.
    pub token: String,
    /// `sha256:` of the installation CA, from the join command.
    pub ca_fingerprint: String,
    /// What the machine calls itself.
    pub hostname: String,
}

/// Enrolls this machine and records its identity.
pub async fn join(state: &StateDir, j: &Join) -> Result<Identity, JoinError> {
    if let Some(node) = state.enrolled_as()? {
        return Err(StateError::AlreadyEnrolled(node).into());
    }
    let key = state.key_or_create()?;
    let (config, pin) = tls::pinned(&j.ca_fingerprint)?;
    let uri: http::Uri =
        j.server.parse().map_err(|e: http::uri::InvalidUri| JoinError::Server(e.to_string()))?;
    let client = agent::AgentServiceClient::new(
        HttpClient::with_tls(Arc::new(config)),
        ClientConfig::new(uri),
    );
    let request = agent::EnrollRequest {
        enrollment_token: j.token.clone(),
        node_public_key: key.verifying_key().to_bytes().to_vec(),
        hostname: j.hostname.clone(),
        ..Default::default()
    };
    let enrolled = client.enroll(request).await?.into_owned();
    let identity = Identity {
        node: enrolled.node.clone(),
        server: j.server.clone(),
        key,
        ca_pem: pin.ca_pem().ok_or(JoinError::NoCa)?,
        keys: enrolled,
    };
    state.save(&identity)?;
    Ok(identity)
}
