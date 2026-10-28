// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's part in its workloads' migrations (§4.8 of docs/02-architecture.md, §8.1 of
//! docs/05-scheduling.md). Each side's assignment of a migrating workload carries the
//! migration's role: which side, which phase, the peer, what crosses for each volume, and, on
//! the destination once the placement moved, the streams' watermarks. The side the workload is
//! not placed on authoritatively, the destination before the cutover and the source after it,
//! is a standby: told the target and its volumes, held stopped, executing no plan, and holding
//! no endpoint, run, or challenge of its node. The source is told the workload stopped from
//! `quiesce` on, which is what quiescing is.

use super::Desired;
use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::workload::PowerState;
use korpis_core::workload::migration::{MigrationRole, Phase, Side};
use std::collections::BTreeMap;
use uuid::Uuid;

/// `node`'s role in each migration it takes part in, by workload.
pub(super) type Roles = BTreeMap<Uuid, MigrationRole>;

/// Makes `d` carry `role`: held stopped from `quiesce` on the source, and a standby where the
/// workload is not placed on its node.
pub(super) fn with_role(mut d: Desired, role: &MigrationRole, standby: bool) -> Desired {
    if standby || (role.side == Side::Source && role.phase >= Phase::Quiesce) {
        d.assignment.power.power.desired = PowerState::Stopped;
    }
    if standby {
        (d.assignment.plan, d.assignment.retire, d.plan) = (None, None, None);
        d.standby = true;
    }
    d.assignment.migration = Some(role.clone());
    d
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// `node`'s role in each migration it takes part in that has not ended.
    pub(super) async fn roles_on(&self, node: Uuid) -> Result<Roles, AppError> {
        let mut roles = Roles::new();
        for (m, volumes, watermarks) in self.store.migrations_on(node).await? {
            let Some(side) = Side::of(&m, node) else { continue };
            let role = MigrationRole::of(&m, side, (&volumes, &watermarks));
            roles.insert(m.workload, role);
        }
        Ok(roles)
    }
}
