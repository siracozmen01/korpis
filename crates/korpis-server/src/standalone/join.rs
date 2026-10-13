// SPDX-License-Identifier: AGPL-3.0-or-later

//! Joining (§3.1 of docs/02-architecture.md). The relay creates its keys before it sends anything,
//! so a join whose answer was lost is asked again with the same keys and answered with the same
//! relay; it trusts the control plane only once the installation's authority, presented behind
//! the control plane's certificate, has the fingerprint the join command carries.

use super::RelayDir;
use connectrpc::client::{ClientConfig, HttpClient};
use korpis_proto::korpis::relay::v1 as relay;
use rcgen::KeyPair;
use std::sync::Arc;

/// Why a relay could not join.
#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    /// It has joined already.
    #[error("this relay has joined already, as {0}; a relay joins once")]
    Joined(String),
    /// Its directory could not be read or written.
    #[error("the relay directory: {0}")]
    Io(#[from] std::io::Error),
    /// A key could not be made or read.
    #[error("a key: {0}")]
    Key(String),
    /// The control plane could not be reached or trusted.
    #[error("the control plane: {0}")]
    Connect(String),
    /// The control plane refused.
    #[error("the control plane refused: {0}")]
    Refused(String),
}

/// The relay's identity key, created on first use.
fn identity(dir: &RelayDir) -> Result<ed25519_dalek::SigningKey, JoinError> {
    let seed = dir.secret_once("identity.key", || {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).map_err(|e| e.to_string())?;
        Ok(b.to_vec())
    })?;
    let seed: [u8; 32] =
        seed.try_into().map_err(|_| JoinError::Key("identity.key is malformed".into()))?;
    Ok(ed25519_dalek::SigningKey::from_bytes(&seed))
}

/// The relay's TLS key, P-256, created on first use.
fn tls_key(dir: &RelayDir) -> Result<KeyPair, JoinError> {
    let pem = dir.secret_once("tls.key", || {
        KeyPair::generate().map(|k| k.serialize_pem().into_bytes()).map_err(|e| e.to_string())
    })?;
    let pem = String::from_utf8(pem).map_err(|_| JoinError::Key("tls.key is not PEM".into()))?;
    KeyPair::from_pem(&pem).map_err(|e| JoinError::Key(format!("tls.key: {e}")))
}

/// Joins the control plane at `server` with `token`, pinning the authority with `fingerprint`;
/// the relay's identifier.
pub async fn join(
    dir: &RelayDir,
    server: &str,
    token: &str,
    fingerprint: &str,
) -> Result<String, JoinError> {
    if let Some(id) = dir.joined()? {
        return Err(JoinError::Joined(id));
    }
    dir.create()?;
    let identity = identity(dir)?;
    let tls = tls_key(dir)?;
    let (config, pinned) =
        korpis_tls::pinned(fingerprint).map_err(|e| JoinError::Connect(e.to_string()))?;
    let uri =
        server.parse().map_err(|e: http::uri::InvalidUri| JoinError::Connect(e.to_string()))?;
    let client = relay::RelayControlServiceClient::new(
        HttpClient::with_tls(Arc::new(config)),
        ClientConfig::new(uri),
    );
    let request = relay::JoinRequest {
        enrollment_token: token.to_owned(),
        identity_key: identity.verifying_key().to_bytes().to_vec(),
        tls_key: tls.public_key_raw().to_vec(),
        ..Default::default()
    };
    let joined = client.join(request).await.map_err(|e| JoinError::Refused(e.to_string()))?;
    let joined = joined.into_owned();
    let ca = pinned.ca_pem().ok_or_else(|| JoinError::Connect("no authority was pinned".into()))?;
    dir.public("ca.pem", &ca)?;
    dir.public("tls.pem", joined.certificate_chain_pem.as_bytes())?;
    let expires = joined.certificate_expires_at.as_option().map_or(0, |t| t.seconds);
    dir.public("tls.expires", expires.to_string().as_bytes())?;
    dir.public("server", server.as_bytes())?;
    dir.public("url", joined.url.as_bytes())?;
    dir.public("relay", joined.relay.as_bytes())?;
    Ok(joined.relay)
}
