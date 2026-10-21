// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the agent keeps on disk under its state directory, opened as it starts.

use super::RunError;
use crate::inventory::Inventory;
use crate::journal::{Bounds, JournalHandle};
use crate::network::{Addresses, Networking, Pools};
use crate::reconcile::progress::Progress;
use crate::reconcile::streams::Streams;
use crate::state::StateDir;

/// What the agent keeps on disk under its state directory.
pub(super) struct Kept {
    pub(super) journal: JournalHandle,
    pub(super) progress: Progress,
    pub(super) streams: Streams,
    pub(super) inventory: Inventory,
    pub(super) network: Option<Networking>,
}

impl Kept {
    /// Opens each under `state`, the journal within `bounds`, and the workloads' addresses
    /// where the node gives them networks from `pools`.
    pub(super) fn open(
        state: &StateDir,
        bounds: Bounds,
        pools: Option<Pools>,
    ) -> Result<Self, RunError> {
        let io = |what| move |source| RunError::Io { what, source };
        let root = state.root();
        Ok(Self {
            journal: JournalHandle::open(&root.join("journal"), bounds)
                .map_err(io("the journal"))?,
            progress: Progress::open(&root.join("progress")).map_err(io("the progress"))?,
            streams: Streams::open(&root.join("streams")).map_err(io("the streams watched"))?,
            inventory: Inventory::open(&root.join("inventory"))
                .map_err(io("the content inventory"))?,
            network: match pools {
                Some(pools) => Some(Networking {
                    pools,
                    addresses: Addresses::open(&root.join("addresses"))
                        .map_err(io("the workloads' addresses"))?,
                    resolvers: std::collections::BTreeMap::new(),
                    dependencies_opened: std::collections::BTreeMap::new(),
                }),
                None => None,
            },
        })
    }
}
