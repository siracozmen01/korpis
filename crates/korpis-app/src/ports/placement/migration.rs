// SPDX-License-Identifier: AGPL-3.0-or-later

//! Migrations (§8.1 of docs/05-scheduling.md): the record each keeps, its volumes, and how it
//! ended.

use crate::ports::StoreResult;
use korpis_core::workload::migration::{Migration, MigrationVolume, Outcome};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading migrations.
pub trait MigrationRead {
    /// The migrations of `workload`, newest first, each with its volumes.
    fn migrations_of(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<(Migration, Vec<MigrationVolume>)>>> + Send;

    /// The migration `id`, with its volumes.
    fn migration(
        &self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<(Migration, Vec<MigrationVolume>)>>> + Send;
}

/// Keeping migrations.
pub trait MigrationWrite {
    /// Records `m`, a new migration, with `volumes`.
    fn insert_migration(
        &mut self,
        m: &Migration,
        volumes: &[MigrationVolume],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// The migration of `workload` that has not ended, locked for the rest of the transaction.
    fn live_migration_in(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Migration>>> + Send;

    /// The migration `id`, locked for the rest of the transaction.
    fn migration_in(
        &mut self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Migration>>> + Send;

    /// Frees the placement of `workload` on `node`, a migration's destination it no longer goes
    /// to, with what it held there; whether there was one that was not authoritative.
    fn unbind_destination(
        &mut self,
        workload: Uuid,
        node: Uuid,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Ends the migration `id`, which has not ended, as `outcome` for `reason` at `at`; whether
    /// it had not.
    fn end_migration(
        &mut self,
        id: Uuid,
        ended: (Outcome, &str),
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
