// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an operation is over, item by item: found when it is proposed, so the proposer and the
//! approver read what it will touch, and found again when it is approved, since a workload may
//! have been placed on the node or left it meanwhile. One that arrived is added; one gone is
//! skipped. Nothing arrives after approval: the node is draining, and takes no placement.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, NodeWrite as _, RecipeSource, Store};
use korpis_core::operation::{Item, ItemState, Kind, Operation, Scope};
use std::time::SystemTime;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The subjects `op` is over now, each with what it was found to need.
    async fn subjects(
        &self,
        tx: &mut S::Tx,
        op: &Operation,
    ) -> Result<Vec<(Uuid, serde_json::Map<String, serde_json::Value>)>, AppError> {
        match op.scope {
            Scope::Node(node) => {
                let (mut placed, _) = tx.node_holdings(node).await?;
                placed.sort_unstable();
                let mut found = Vec::with_capacity(placed.len());
                for w in placed {
                    let detail = match op.kind {
                        Kind::Evacuate => self.evacuation(w, node).await?.detail(),
                        Kind::Drain | Kind::TemplateRollout => self.named(w).await?,
                    };
                    found.push((w, detail));
                }
                Ok(found)
            }
            Scope::Template { template, version } => self.rollout_subjects(template, version).await,
        }
    }

    /// A workload's name, as an item's detail.
    async fn named(&self, w: Uuid) -> Result<serde_json::Map<String, serde_json::Value>, AppError> {
        let name = self.store.workload(w).await?.map(|w| w.name.to_string()).unwrap_or_default();
        Ok(serde_json::Map::from_iter([("name".to_owned(), name.into())]))
    }

    /// The items of `op`, a new operation.
    pub(super) async fn items_of(
        &self,
        tx: &mut S::Tx,
        op: &Operation,
    ) -> Result<Vec<Item>, AppError> {
        let found = self.subjects(tx, op).await?;
        Ok(found
            .into_iter()
            .zip(0..)
            .map(|((subject, detail), n)| Item { detail, ..Item::waiting(op.id, subject, n) })
            .collect())
    }

    /// `items`, found again as `op` is approved at `now`: each subject in scope now and not yet
    /// an item added after the rest, and each waiting item no longer in scope skipped.
    pub(super) async fn items_again(
        &self,
        tx: &mut S::Tx,
        op: &Operation,
        items: &mut Vec<Item>,
        now: SystemTime,
    ) -> Result<(), AppError> {
        let found = self.subjects(tx, op).await?;
        for item in items.iter_mut().filter(|i| i.state == ItemState::Waiting) {
            if !found.iter().any(|(s, _)| *s == item.subject) {
                item.end(ItemState::Skipped, "no longer in scope when it was approved", now);
            }
        }
        let mut next = items.iter().map(|i| i.position + 1).max().unwrap_or(0);
        for (subject, detail) in found {
            if !items.iter().any(|i| i.subject == subject) {
                items.push(Item { detail, ..Item::waiting(op.id, subject, next) });
                next += 1;
            }
        }
        Ok(())
    }
}
