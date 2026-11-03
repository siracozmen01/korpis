// SPDX-License-Identifier: AGPL-3.0-or-later

//! The migrations a removed node takes part in, ended as its removal begins, since the node will
//! never report another phase. Before the workload moved, each is abandoned: the workload is where
//! it was, and the destination's placement is freed. After the move, a removed destination rolls
//! the workload back to the source, which still holds its copies until a minute before its hold
//! ends; past that, or where the source is the node removed, the migration is complete, and what
//! the removed node held is lost with it, as the removal says of every workload it held.

use super::conclude::EndedBy;
use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, MigrationWrite as _, RecipeSource, Store};
use korpis_core::workload::migration::{CLOCK_MARGIN, Migration};
use uuid::Uuid;

/// What a removal of `node` does with `m`, live, at `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    Abandon,
    RollBack,
    Complete,
}

fn ends(m: &Migration, node: Uuid, now: std::time::SystemTime) -> Ends {
    let rollable = m.hold_until.is_none_or(|h| now + CLOCK_MARGIN < h);
    match (m.moved_at.is_some(), node == m.destination) {
        (false, _) => Ends::Abandon,
        (true, true) if rollable => Ends::RollBack,
        (true, _) => Ends::Complete,
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Ends every migration node `node`, being removed, takes part in; how many.
    pub(crate) async fn end_migrations_on(
        &self,
        tx: &mut S::Tx,
        node: Uuid,
    ) -> Result<usize, AppError> {
        let mut ended = 0;
        for (seen, _, _) in self.store.migrations_on(node).await? {
            let Some(m) = tx.migration_in(seen.id).await?.filter(Migration::live) else { continue };
            let side = if node == m.source { "source" } else { "destination" };
            let why = format!("the {side} node was removed");
            match ends(&m, node, self.clock.now()) {
                Ends::Abandon => self.abandon_in(tx, &m, (&why, EndedBy::Failure)).await?,
                Ends::RollBack => self.roll_back_in(tx, &m, (&why, EndedBy::Failure)).await?,
                Ends::Complete => self.complete_migration(tx, &m).await?,
            }
            ended += 1;
        }
        Ok(ended)
    }
}
