// SPDX-License-Identifier: AGPL-3.0-or-later

//! The per-node journal cursor (§4.10 of docs/02-architecture.md).

use super::StoreResult;
use std::future::Future;
use uuid::Uuid;

/// Reading the cursor.
pub trait JournalRead {
    /// The highest sequence ingested from `node`; zero before the first.
    fn journal_ingested(&self, node: Uuid) -> impl Future<Output = StoreResult<u64>> + Send;
}

/// Moving the cursor, in the transaction that ingests.
pub trait JournalWrite {
    /// The highest sequence ingested from `node`, locked until the transaction ends.
    fn lock_journal(&mut self, node: Uuid) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Moves the cursor to `through`.
    fn advance_journal(
        &mut self,
        node: Uuid,
        through: u64,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
