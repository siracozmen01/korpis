// SPDX-License-Identifier: AGPL-3.0-or-later

//! The relay role in the control plane's own process (§3.1 of docs/02-architecture.md): it
//! verifies tokens by the keys the process loaded and the revocation list it publishes, and a
//! node's `DataHello` against the database, whose nonce memory every replica shares.

use super::Verifier;
use crate::api::{ServerApp, error};
use crate::revocations::Revocations;
use connectrpc::ConnectError;
use korpis_app::node::NodeProof;
use korpis_core::authz::route::RouteRing;
use korpis_core::authz::token::KeyRing;
use std::collections::HashSet;
use std::sync::{Arc, PoisonError, RwLock};
use uuid::Uuid;

/// What the in-process role verifies by.
#[derive(Debug)]
pub struct InProcess {
    app: Arc<ServerApp>,
    revocations: Arc<Revocations>,
    keys: RwLock<KeyRing>,
}

impl InProcess {
    /// The role over `app`, judging by the lists `revocations` publishes.
    #[must_use]
    pub fn new(app: Arc<ServerApp>, revocations: Arc<Revocations>) -> Self {
        Self { app, revocations, keys: RwLock::default() }
    }

    /// Trusts `keys` for the tokens clients present.
    pub fn trust(&self, keys: KeyRing) {
        *self.keys.write().unwrap_or_else(PoisonError::into_inner) = keys;
    }
}

impl Verifier for InProcess {
    fn keys(&self) -> KeyRing {
        self.keys.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn routes(&self) -> RouteRing {
        RouteRing::new(self.app.route_keys())
    }

    fn revoked(&self) -> HashSet<Uuid> {
        self.revocations.revoked()
    }

    async fn attach(&self, node: Uuid, proof: &NodeProof, relay: &str) -> Result<(), ConnectError> {
        self.app.attach_node(node, proof, relay).await.map_err(error::from_app)
    }
}
