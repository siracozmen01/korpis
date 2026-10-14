// SPDX-License-Identifier: AGPL-3.0-or-later

//! Repositories, the sealed keys their backups are encrypted under, and backups (§5 of
//! docs/06-storage.md).

use super::StoreResult;
use crate::page::Page;
use korpis_core::backup::{Backup, Repository};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// A key as the vault sealed it, and which of the vault's keys sealed it.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedKey {
    /// The vault key that sealed it.
    pub sealed_by: String,
    /// The sealed bytes.
    pub sealed: Vec<u8>,
}

impl std::fmt::Debug for SealedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedKey").field("sealed_by", &self.sealed_by).finish_non_exhaustive()
    }
}

/// Reading repositories and backups.
pub trait BackupRead {
    /// A repository, deleted or not.
    fn repository(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Repository>>> + Send;

    /// An organization's live repositories, or only those scoped to `project` and to the whole
    /// organization, after the page's cursor.
    fn repositories(
        &self,
        organization: Uuid,
        project: Option<Uuid>,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Repository>>> + Send;

    /// A backup, pruned or not.
    fn backup(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Backup>>> + Send;

    /// Where the console streams of a workload of `project` in `organization` are offloaded: the
    /// project's repository that takes streams, or else the organization's (§4 of
    /// docs/14-streams.md).
    fn offload_repository(
        &self,
        organization: Uuid,
        project: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Repository>>> + Send;

    /// Whether a live repository already takes the streams of `organization` as a whole, or of
    /// `project` in it.
    fn streams_taken(
        &self,
        organization: Uuid,
        project: Option<Uuid>,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Whether a backup of `workload` into `repository` is pending under a plan still open:
    /// one writer per workload prefix (§5.5 of docs/06-storage.md).
    fn backup_open(
        &self,
        repository: Uuid,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// The newest succeeded, unpruned backup of `volume` in `repository`.
    fn last_backup(
        &self,
        repository: Uuid,
        volume: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Uuid>>> + Send;

    /// A workload's backups, newest first after the page's cursor; pruned ones only when
    /// `deleted`.
    fn backups(
        &self,
        workload: Uuid,
        deleted: bool,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Backup>>> + Send;
}

/// Writing repositories, keys, and backups.
pub trait BackupWrite {
    /// Adds a repository, its key-encryption key sealed as `kek`; refused as
    /// [`Unique::RepositoryPrefix`](super::Unique::RepositoryPrefix) when a live repository of
    /// the organization and provider name has a place its prune could reach into, checked under
    /// a lock that two additions take in turn.
    fn insert_repository(
        &mut self,
        r: &Repository,
        kek: &SealedKey,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// A repository's sealed key-encryption key.
    fn repository_key(
        &mut self,
        repository: Uuid,
    ) -> impl Future<Output = StoreResult<Option<SealedKey>>> + Send;

    /// `workload`'s sealed data key in `repository`, if it has one.
    fn workload_key(
        &mut self,
        repository: Uuid,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<SealedKey>>> + Send;

    /// Keeps `dek` as `workload`'s data key in `repository` unless it has one; the key it has
    /// afterwards, which is the earlier one when two raced.
    fn keep_workload_key(
        &mut self,
        repository: Uuid,
        workload: Uuid,
        dek: &SealedKey,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<SealedKey>> + Send;

    /// Adds a backup, pending.
    fn insert_backup(&mut self, b: &Backup) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records how a pending backup ended; false if it was not pending.
    fn finish_backup(&mut self, b: &Backup) -> impl Future<Output = StoreResult<bool>> + Send;
}

/// Why the vault could not seal or open a key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    /// The sealed key does not open under the key it names, in this context.
    #[error("a sealed key does not open: {0}")]
    Forged(String),
    /// It names a vault key this control plane does not hold.
    #[error("no vault key {0}")]
    UnknownKey(String),
    /// Randomness or the cipher failed.
    #[error("{0}")]
    Failed(String),
}

/// A workload's data key, opened for one job; wiped when dropped.
pub struct DataKey(pub zeroize::Zeroizing<[u8; 32]>);

impl std::fmt::Debug for DataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DataKey(..)")
    }
}

/// Where repository keys are held (§5.5 of docs/06-storage.md). A key-encryption key never
/// leaves the vault: it generates one sealed, generates data keys sealed under it, and opens a
/// data key for a job. Each seal binds what it is for, so a sealed key moved to another
/// repository or workload does not open.
pub trait KeyVault: Send + Sync + std::fmt::Debug {
    /// A new key-encryption key for `repository`, sealed.
    fn new_kek(&self, repository: Uuid) -> Result<SealedKey, VaultError>;

    /// A new data key for `workload` in `repository`, sealed under the repository's `kek`.
    fn new_dek(
        &self,
        kek: &SealedKey,
        repository: Uuid,
        workload: Uuid,
    ) -> Result<SealedKey, VaultError>;

    /// `workload`'s data key in `repository`, opened from `dek` under `kek`.
    fn open_dek(
        &self,
        kek: &SealedKey,
        dek: &SealedKey,
        repository: Uuid,
        workload: Uuid,
    ) -> Result<DataKey, VaultError>;
}
