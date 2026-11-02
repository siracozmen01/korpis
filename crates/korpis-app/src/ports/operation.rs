// SPDX-License-Identifier: AGPL-3.0-or-later

//! Operations and their items.

use super::StoreResult;
use crate::page::Page;
use korpis_core::operation::{Item, Operation};
use std::future::Future;
use uuid::Uuid;

/// An operation with its items, in the order they begin in.
pub type OperationAt = (Operation, Vec<Item>);

/// Reading operations.
pub trait OperationRead {
    /// The operation `id`, with its items.
    fn operation(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<OperationAt>>> + Send;

    /// Operations, newest first, after the page's cursor; only those over `node`, if named.
    fn operations(
        &self,
        node: Option<Uuid>,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Operation>>> + Send;

    /// Every operation approved and not ended: those its reconciler tends.
    fn live_operations(&self) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;
}

/// Keeping operations.
pub trait OperationWrite {
    /// Records `op`, a new operation, with `items`. A node already under an operation that has
    /// not ended is [`super::StoreError::Duplicate`].
    fn insert_operation(
        &mut self,
        op: &Operation,
        items: &[Item],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// The operation `id`, with its items, locked for the rest of the transaction.
    fn operation_in(
        &mut self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<OperationAt>>> + Send;

    /// Writes `op` over the version it was read at, raising it; whether it was still that.
    fn update_operation(
        &mut self,
        op: &Operation,
        read_at: u64,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Writes `item`, adding it when the operation has no item about its subject.
    fn put_item(&mut self, item: &Item) -> impl Future<Output = StoreResult<()>> + Send;
}
