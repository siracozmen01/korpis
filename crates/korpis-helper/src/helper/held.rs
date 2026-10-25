// SPDX-License-Identifier: AGPL-3.0-or-later

//! The dedicated cores verb (§5.2 of docs/05-scheduling.md): the workloads' cpusets kept to the
//! cores the node's placements hold, under the drivers' cgroup parents the helper keeps.

use super::Helper;
use crate::cores::Cores;
use crate::error::{HelperError, HelperResult};
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;
use std::collections::{BTreeMap, BTreeSet};

impl Helper {
    /// The same helper, keeping the workloads' cpusets as `cores` says.
    #[must_use]
    pub fn with_cores(mut self, cores: Cores) -> Self {
        self.cores = Some(cores);
        self
    }

    /// Keeps the cpusets to the cores `r` says are held.
    pub(super) fn hold_cores(&self, r: &helper::HoldCoresRequest) -> HelperResult<Answer> {
        let cores = self
            .cores
            .as_ref()
            .ok_or_else(|| HelperError::Refused("this helper keeps no cpusets".into()))?;
        let held: BTreeMap<String, BTreeSet<u32>> = r
            .parents
            .iter()
            .map(|h| (h.parent.clone(), h.cpus.iter().copied().collect()))
            .collect();
        if held.len() != r.parents.len() {
            return Err(HelperError::Refused("a cgroup parent named twice".into()));
        }
        let shared = cores.hold(&held)?;
        Ok(Answer::HoldCores(Box::new(helper::HoldCoresResponse { shared, ..Default::default() })))
    }

    /// The same helper, keeping the node's side of the mesh as `overlay` says.
    #[must_use]
    pub fn with_overlay(mut self, overlay: crate::overlay::Overlay) -> Self {
        self.overlay = Some(overlay);
        self
    }

    /// Keeps the node's side of the mesh to `r`.
    pub(super) fn keep_overlay(&self, r: &helper::KeepOverlayRequest) -> HelperResult<Answer> {
        let overlay = self
            .overlay
            .as_ref()
            .ok_or_else(|| HelperError::Refused("this helper keeps no overlay".into()))?;
        Ok(Answer::KeepOverlay(Box::new(overlay.keep(r)?)))
    }
}
