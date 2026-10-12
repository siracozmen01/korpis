// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota held at admission (§5.1 of docs/05-scheduling.md).

use super::StoreResult;
use korpis_core::tenancy::quota::{Held, QuotaEntry, QuotaOwner};
use std::collections::BTreeMap;
use std::future::Future;
use uuid::Uuid;

/// A workload as quota sees it: its labels, for selectors, and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// The workload.
    pub workload: Uuid,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
    /// The larger of what its head declares and its target runs.
    pub held: Held,
}

/// Admission's side of quota. Every method runs in the transaction of the declaration it admits.
pub trait QuotaWrite {
    /// The entries of `project` and of `organizations`, locked until the transaction ends, in
    /// identifier order so that two admissions never wait on each other in a cycle. Locking
    /// them is what makes the check hold: a concurrent admission against the same entries waits
    /// for this one and then sees what it admitted.
    fn lock_quota(
        &mut self,
        project: Uuid,
        organizations: &[Uuid],
    ) -> impl Future<Output = StoreResult<Vec<QuotaEntry>>> + Send;

    /// Every workload not deleted under `owner` (an organization's includes its descendants'),
    /// except `except`.
    fn holders(
        &mut self,
        owner: QuotaOwner,
        except: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<Holder>>> + Send;

    /// Bumps the version of every entry in `entries`: the record, in the admitting
    /// transaction, that they were consumed against (§5.1).
    fn consume_quota(&mut self, entries: &[Uuid]) -> impl Future<Output = StoreResult<()>> + Send;
}
