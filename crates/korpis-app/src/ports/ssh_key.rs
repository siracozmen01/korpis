// SPDX-License-Identifier: AGPL-3.0-or-later

//! The SSH keys people register for SFTP (§9.9 of docs/08-identity.md).

use super::StoreResult;
use korpis_core::account::ssh_key::SshKey;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading SSH keys.
pub trait SshKeyRead {
    /// The keys of `account`, oldest first.
    fn ssh_keys(&self, account: Uuid) -> impl Future<Output = StoreResult<Vec<SshKey>>> + Send;

    /// The key whose SHA-256 fingerprint is `fingerprint`, as `ssh-keygen -l` prints it.
    fn ssh_key_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> impl Future<Output = StoreResult<Option<SshKey>>> + Send;
}

/// Changing SSH keys.
pub trait SshKeyWrite {
    /// Adds `key`; a duplicate if its fingerprint is registered already.
    fn insert_ssh_key(&mut self, key: &SshKey) -> impl Future<Output = StoreResult<()>> + Send;

    /// Removes key `id` of `account`; false if it has none such.
    fn remove_ssh_key(
        &mut self,
        account: Uuid,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records that key `id` authorized a file session at `at`.
    fn ssh_key_used(
        &mut self,
        id: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
