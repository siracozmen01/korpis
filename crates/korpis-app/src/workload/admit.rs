// SPDX-License-Identifier: AGPL-3.0-or-later

//! Admission (§5.1 of docs/05-scheduling.md): a declaration fits every quota entry of its
//! project and of every organization above it that counts it, read from the totals kept beside
//! each entry, and the entries are held, locked and consumed, in the transaction that writes it.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, QuotaWrite, RecipeSource, Store};
use korpis_core::tenancy::quota::{Exceeded, Held};
use korpis_core::workload::Workload;
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

    /// Admits `workload`, which held `old` (nothing, if new) and will hold `new`, against the
    /// totals of every entry counting it, less what each already counted of it.
    pub(super) async fn admit(
        &self,
        tx: &mut S::Tx,
        workload: &Workload,
        old: Option<&Held>,
        new: &Held,
    ) -> Result<(), AppError> {
        let entries = self.locked_entries(tx, workload.project, workload.organization).await?;
        let counted = tx.counted(workload.id).await?.unwrap_or_default();
        let labels = workload.labels.as_map();
        let counting: Vec<_> = entries.iter().filter(|e| e.counts(labels)).collect();
        for e in &counting {
            let mut rest = e.held.clone().unwrap_or_default();
            if counted.by.contains(&e.id) {
                rest.sub(&counted.held);
            }
            e.admit(&rest, old, new).map_err(|x| exceeded(&x))?;
        }
        let ids: Vec<Uuid> = counting.iter().map(|e| e.id).collect();
        if !ids.is_empty() {
            tx.consume_quota(&ids).await?;
        }
        Ok(())
    }
}
