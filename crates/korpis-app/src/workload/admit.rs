// SPDX-License-Identifier: AGPL-3.0-or-later

//! Admission (§5.1 of docs/05-scheduling.md): a declaration fits every quota entry of its
//! project and of every organization above it that counts it, and the entries are held, locked
//! and consumed, in the transaction that writes it.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, QuotaWrite, RecipeSource, Store};
use korpis_core::tenancy::quota::{Exceeded, Held, QuotaOwner};
use korpis_core::workload::Workload;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use uuid::Uuid;

/// The deepest an organization tree is walked; a cycle is impossible by schema, so reaching it
/// means corrupt data.
const MAX_DEPTH: usize = 64;

fn exceeded(e: &Exceeded) -> AppError {
    AppError::QuotaExceeded { dimension: e.dimension.as_str(), message: e.to_string() }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The organization and every organization above it, nearest first.
    pub(crate) async fn organization_chain(
        &self,
        organization: Uuid,
    ) -> Result<Vec<Uuid>, AppError> {
        let mut chain = Vec::new();
        let mut next = Some(organization);
        while let Some(id) = next {
            if chain.len() >= MAX_DEPTH {
                return Err(AppError::Internal(format!("organization {id} is too deep")));
            }
            let org = self
                .store
                .organization(id)
                .await?
                .ok_or_else(|| AppError::Internal(format!("organization {id} vanished")))?;
            chain.push(org.id);
            next = org.parent;
        }
        Ok(chain)
    }

    /// Admits `workload`, which held `old` (nothing, if new) and will hold `new`.
    pub(super) async fn admit(
        &self,
        tx: &mut S::Tx,
        workload: &Workload,
        old: Option<&Held>,
        new: &Held,
    ) -> Result<(), AppError> {
        let organizations = self.organization_chain(workload.organization).await?;
        let labels = workload.labels.as_map();
        let entries = tx.lock_quota(workload.project, &organizations).await?;
        let counting: Vec<_> = entries.into_iter().filter(|e| e.counts(labels)).collect();
        let mut by_owner = BTreeMap::new();
        for e in &counting {
            let key = match e.owner {
                QuotaOwner::Organization(o) => (0u8, o),
                QuotaOwner::Project(p) => (1u8, p),
            };
            let holders = match by_owner.entry(key) {
                Entry::Occupied(o) => o.into_mut(),
                Entry::Vacant(v) => v.insert(tx.holders(e.owner, workload.id).await?),
            };
            let others: Vec<Held> =
                holders.iter().filter(|h| e.counts(&h.labels)).map(|h| h.held.clone()).collect();
            e.admit(&others, old, new).map_err(|x| exceeded(&x))?;
        }
        let ids: Vec<Uuid> = counting.iter().map(|e| e.id).collect();
        if !ids.is_empty() {
            tx.consume_quota(&ids).await?;
        }
        Ok(())
    }
}
