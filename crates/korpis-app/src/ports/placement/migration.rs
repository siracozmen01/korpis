// SPDX-License-Identifier: AGPL-3.0-or-later

//! Migrations (§8.1 of docs/05-scheduling.md): the record each keeps, its volumes, and how it
//! ended.

use crate::ports::StoreResult;
use korpis_core::workload::migration::{
    Migration, MigrationVolume, NewPass, Outcome, Phase, Side, VolumeProgress, Watermark,
};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// A migration with its volumes and the streams' watermarks the source reported.
pub type MigrationAt = (Migration, Vec<MigrationVolume>, Vec<Watermark>);

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

    /// Every migration that has not ended and that `node` takes part in, with its volumes and
    /// the streams' watermarks the source reported.
    fn migrations_on(
        &self,
        node: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<MigrationAt>>> + Send;

    /// Every migration that has not ended.
    fn live_migrations(&self) -> impl Future<Output = StoreResult<Vec<Migration>>> + Send;
}

/// Keeping migrations.
pub trait MigrationWrite: MigrationMoves {
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

    /// Frees the placement of `workload` on `node`, a migration's side it no longer needs, with
    /// what it held there; whether there was one that was not authoritative.
    fn free_placement(
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

/// Recording what a migration's sides report, moving its phases, and moving its placement.
pub trait MigrationMoves {
    /// The volumes of the migration `id`, in this transaction.
    fn migration_volumes_in(
        &mut self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<MigrationVolume>>> + Send;

    /// Records what `side` reported of each volume of the migration `id`: on the source what the
    /// pass under way sent, on the destination that it holds the pass under way and its
    /// identifier there, and either side's manifest; where both sides' manifests are in, where
    /// they first differ. What it says of a pass not under way is not recorded.
    fn record_volume_progress(
        &mut self,
        id: Uuid,
        side: Side,
        volumes: &[VolumeProgress],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Starts the pass `p` of a volume of the migration `id` at `at`: what either side reported of
    /// the pass before it is of no pass under way.
    fn start_pass(
        &mut self,
        id: Uuid,
        p: NewPass,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records the streams' watermarks the source reported for the migration `id`.
    fn record_watermarks(
        &mut self,
        id: Uuid,
        watermarks: &[Watermark],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that `side` reported `phase` done for the migration `id`.
    fn record_reported(
        &mut self,
        id: Uuid,
        side: Side,
        phase: Phase,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Moves the migration `id` on to `phase` at the first of `times`, its hold ending at the
    /// second where set.
    fn advance_migration(
        &mut self,
        id: Uuid,
        phase: Phase,
        times: (SystemTime, Option<SystemTime>),
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Moves the authoritative placement of the workload of `migration`, its id and its
    /// workload's, between `nodes`, from the first to the second, bound at `at` by migration,
    /// and each of `volumes` with it onto the second under the identifier given; records on the
    /// migration that it moved at `at`, or, moving back to its source, that it no longer has.
    fn move_placement(
        &mut self,
        migration: (Uuid, Uuid),
        nodes: (Uuid, Uuid),
        volumes: &[(Uuid, String)],
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Keeps `volume`'s `copy`, on a node under a helper identifier, as a volume of its own,
    /// retained and held by no workload, diverged from it at the first of `times`, `volume`
    /// holding what was written up to the second: a migration rolled back leaves it so, as a
    /// failover onto a replica does (§2.2 of docs/06-storage.md).
    fn keep_divergent(
        &mut self,
        volume: Uuid,
        copy: (Uuid, &str),
        times: (SystemTime, SystemTime),
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
