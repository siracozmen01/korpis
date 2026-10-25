// SPDX-License-Identifier: AGPL-3.0-or-later

//! The mesh between nodes (§9.1 of docs/07-networking.md), as the control plane judges it from
//! what every node reports: its members, the nodes outside it, and whether a node reaches every
//! other member, which is what `Node.overlay` says.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::node::mesh::{Candidate, Mesh, Pairs, Standing, mesh};
use uuid::Uuid;

/// The mesh and what its candidates report, judged once for every node one request renders.
pub(crate) struct Judged {
    mesh: Mesh,
    candidates: Vec<Candidate>,
    now: std::time::SystemTime,
}

impl Judged {
    /// `node`'s place in the mesh: whether it reaches every other member, its side as it
    /// reported it, and each pair with it.
    pub(crate) fn standing(&self, node: Uuid) -> Standing {
        Standing::of(node, (&self.mesh, &self.candidates), self.now)
    }

    /// The members' nodes.
    pub(crate) fn member_nodes(&self) -> std::collections::BTreeSet<Uuid> {
        self.mesh.members.iter().map(|m| m.node).collect()
    }

    /// The pairs of members that reach each other.
    pub(crate) fn pairs(&self) -> Pairs {
        Pairs::of(&self.mesh, &self.candidates, self.now)
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The mesh as every node reports it now.
    ///
    /// # Errors
    /// The store could not be read.
    pub async fn mesh(&self) -> Result<Mesh, AppError> {
        Ok(mesh(self.store.mesh_candidates().await?))
    }

    /// The mesh judged as every node reports it now.
    pub(crate) async fn judged(&self) -> Result<Judged, AppError> {
        let candidates = self.store.mesh_candidates().await?;
        Ok(Judged { mesh: mesh(candidates.clone()), candidates, now: self.clock.now() })
    }
}
