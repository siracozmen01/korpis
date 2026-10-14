// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backups (§5 of docs/06-storage.md): the repository a workload's backups are kept in, how long
//! they are kept, and each backup as the node that ran it reported it.

use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// Where a repository's key-encryption key is held (§5.5 of docs/06-storage.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCustody {
    /// By the control plane: the operator can read tenants' backups, through a grant naming
    /// the tenant that the tenant sees.
    Operator,
    /// By a key management service the tenant controls, asked per job: the operator cannot.
    Tenant,
}

impl KeyCustody {
    /// The stored and wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Tenant => "tenant",
        }
    }

    /// The custody a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Operator, Self::Tenant].into_iter().find(|c| c.as_str() == s)
    }
}

/// How long backups are kept (§5.3 of docs/06-storage.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Retention {
    /// The most recent backups kept whatever their age.
    pub keep_last: u32,
    /// The last backup of each of this many hours, days, weeks, months, and years.
    pub keep_hourly: u32,
    /// See `keep_hourly`.
    pub keep_daily: u32,
    /// See `keep_hourly`.
    pub keep_weekly: u32,
    /// See `keep_hourly`.
    pub keep_monthly: u32,
    /// See `keep_hourly`.
    pub keep_yearly: u32,
    /// How long a failed backup's record is kept.
    pub keep_failed: Duration,
    /// A floor beneath which no policy, operator, or automation prunes.
    pub min_retention: Duration,
}

impl Retention {
    /// Whether this policy keeps anything: a policy that keeps nothing would let a prune remove
    /// every backup the floor does not protect, so it is refused when set.
    #[must_use]
    pub fn keeps_something(&self) -> bool {
        [
            self.keep_last,
            self.keep_hourly,
            self.keep_daily,
            self.keep_weekly,
            self.keep_monthly,
            self.keep_yearly,
        ]
        .iter()
        .any(|n| *n > 0)
    }
}

/// A repository: a place in a backup target that workloads' backups are kept in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// Its identifier.
    pub id: Uuid,
    /// The organization it belongs to, whose backup target provider it uses.
    pub organization: Uuid,
    /// The project it is scoped to; the whole organization when none.
    pub project: Option<Uuid>,
    /// Unique among the organization's live repositories.
    pub name: String,
    /// The backup target provider's name in the organization.
    pub provider: String,
    /// Its place in the target: a relative prefix ending in `/`.
    pub prefix: String,
    /// Who holds its key-encryption key.
    pub custody: KeyCustody,
    /// How long its backups are kept.
    pub retention: Retention,
    /// When it was made.
    pub created: SystemTime,
    /// When it was deleted.
    pub deleted_at: Option<SystemTime>,
}

impl Repository {
    /// Where `workload`'s objects are in the target: its prefix, which only its jobs write.
    #[must_use]
    pub fn workload_prefix(&self, workload: Uuid) -> String {
        format!("{}workloads/{workload}/", self.prefix)
    }
}

/// Where a backup is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStatus {
    /// Its plan has not finished it.
    Pending,
    /// Its record is in the repository: it is whole.
    Succeeded,
    /// It did not complete; what it wrote is reused by the next.
    Failed,
}

impl BackupStatus {
    /// The stored and wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    /// The status a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Pending, Self::Succeeded, Self::Failed].into_iter().find(|c| c.as_str() == s)
    }
}

/// What a backup read and stored, as its job reported it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BackupStats {
    /// Files read.
    pub files: u64,
    /// Directories read.
    pub directories: u64,
    /// The bytes of every file's data.
    pub bytes: u64,
    /// Chunks this backup stored; the rest were there already.
    pub chunks_new: u64,
    /// Their bytes, before sealing.
    pub bytes_new: u64,
    /// What was written to the store, sealed.
    pub stored_bytes: u64,
    /// Files not read, being unchanged since the parent backup.
    pub files_unchanged: u64,
    /// What was not backed up, and why.
    pub skipped: Vec<String>,
}

/// One backup of one volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    /// Its identifier.
    pub id: Uuid,
    /// The repository it is kept in.
    pub repository: Uuid,
    /// The workload whose volume it is of.
    pub workload: Uuid,
    /// The volume.
    pub volume: Uuid,
    /// The plan that takes it.
    pub plan: Uuid,
    /// The workload's backup whose unchanged files it did not read again.
    pub parent: Option<Uuid>,
    /// Where it is.
    pub status: BackupStatus,
    /// Read from a snapshot, a single point in time; false when read live.
    pub consistent: bool,
    /// What it read and stored.
    pub stats: BackupStats,
    /// Why it failed.
    pub error: Option<String>,
    /// When its plan was proposed.
    pub created: SystemTime,
    /// When its node reported it finished.
    pub finished: Option<SystemTime>,
    /// When it was pruned.
    pub deleted_at: Option<SystemTime>,
}

/// How a backup ended, as the node that ran its job reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedBackup {
    /// The workload whose volume it is of.
    pub workload: Uuid,
    /// The backup.
    pub backup: Uuid,
    /// Whether its record is in the repository.
    pub ok: bool,
    /// Why not.
    pub error: Option<String>,
    /// Whether it was read from a snapshot.
    pub consistent: bool,
    /// What it read and stored.
    pub stats: BackupStats,
}
