// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the agent keeps on its machine (§7 of docs/02-architecture.md): its identity key, which
//! never leaves the node, the node it is, the control plane it dials, the certificate authority
//! it pinned at enrollment, and the keys that sign what it is told.

use buffa::Message as _;
use ed25519_dalek::SigningKey;
use korpis_proto::korpis::agent::v1::EnrollResponse;
use std::path::{Path, PathBuf};

/// Why the state could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// What failed.
        source: std::io::Error,
    },
    /// A file does not hold what it should.
    #[error("{0}: malformed")]
    Malformed(PathBuf),
    /// The agent has not joined a control plane.
    #[error("not enrolled: run `korpis-agent join` first")]
    NotEnrolled,
    /// The agent has joined one already.
    #[error("already enrolled as {0}; a node enrolls once")]
    AlreadyEnrolled(String),
}

/// The agent's enrolled identity.
#[derive(Clone)]
pub struct Identity {
    /// The node, as the control plane names it.
    pub node: String,
    /// Where the control plane is, e.g. `https://korpis.example:7443`.
    pub server: String,
    /// The node identity key.
    pub key: SigningKey,
    /// The installation CA, PEM: the only authority the agent trusts for the control plane.
    pub ca_pem: Vec<u8>,
    /// The verification keys enrollment returned.
    pub keys: EnrollResponse,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("node", &self.node)
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

/// The agent's state directory.
#[derive(Debug, Clone)]
pub struct StateDir {
    root: PathBuf,
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> StateError + '_ {
    move |source| StateError::Io { path: path.to_owned(), source }
}

/// Writes `bytes` to `path` readable by the owner alone, replacing it atomically and durably.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    crate::durable::replace(path, bytes).map_err(io(path))
}

impl StateDir {
    /// The state kept under `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where it is.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// The node this agent enrolled as, if it has.
    pub fn enrolled_as(&self) -> Result<Option<String>, StateError> {
        let path = self.file("node");
        match std::fs::read_to_string(&path) {
            Ok(node) => Ok(Some(node.trim().to_owned())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(&path)(e)),
        }
    }

    /// The node identity key: the one already generated, or a new one, kept before it is ever
    /// sent, so an enrollment whose answer was lost is retried with the same key and the
    /// control plane answers with the same node (§3 of docs/18-operations.md).
    pub fn key_or_create(&self) -> Result<SigningKey, StateError> {
        let path = self.file("node.key");
        match std::fs::read(&path) {
            Ok(bytes) => {
                let key: [u8; 32] =
                    bytes.try_into().map_err(|_| StateError::Malformed(path.clone()))?;
                Ok(SigningKey::from_bytes(&key))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&self.root).map_err(io(&self.root))?;
                let mut seed = [0u8; 32];
                getrandom::fill(&mut seed).map_err(|e| StateError::Io {
                    path: path.clone(),
                    source: std::io::Error::other(e.to_string()),
                })?;
                write_private(&path, &seed)?;
                Ok(SigningKey::from_bytes(&seed))
            }
            Err(e) => Err(io(&path)(e)),
        }
    }

    /// Records the identity, the node last so that a half-written enrollment is not one.
    pub fn save(&self, id: &Identity) -> Result<(), StateError> {
        std::fs::create_dir_all(&self.root).map_err(io(&self.root))?;
        write_private(&self.file("node.key"), id.key.as_bytes())?;
        write_private(&self.file("server"), id.server.as_bytes())?;
        write_private(&self.file("ca.pem"), &id.ca_pem)?;
        write_private(&self.file("keys.binpb"), &id.keys.encode_to_vec())?;
        write_private(&self.file("node"), id.node.as_bytes())
    }

    /// The identity enrollment recorded.
    pub fn load(&self) -> Result<Identity, StateError> {
        let node = self.enrolled_as()?.ok_or(StateError::NotEnrolled)?;
        let read = |name: &str| {
            let path = self.file(name);
            std::fs::read(&path).map_err(io(&path))
        };
        let malformed = |name: &str| StateError::Malformed(self.file(name));
        let key: [u8; 32] = read("node.key")?.try_into().map_err(|_| malformed("node.key"))?;
        let server = String::from_utf8(read("server")?).map_err(|_| malformed("server"))?;
        let keys = EnrollResponse::decode_from_slice(&read("keys.binpb")?)
            .map_err(|_| malformed("keys.binpb"))?;
        Ok(Identity {
            node,
            server: server.trim().to_owned(),
            key: SigningKey::from_bytes(&key),
            ca_pem: read("ca.pem")?,
            keys,
        })
    }
}
