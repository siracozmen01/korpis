// SPDX-License-Identifier: AGPL-3.0-or-later

//! Admission (§5.1 of docs/05-scheduling.md): a declaration fits every quota entry of its
//! project and of every organization above it that counts it, read from the totals kept beside
//! each entry, and the entries are held, locked and consumed, in the transaction that writes it.
//! An organization's entry counts what it promised the organizations beneath it beside what it
//! holds, so a parent's own workloads never take what it guaranteed a child (Finding 139).

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, Counted, QuotaWrite, RecipeSource, Store};
use korpis_core::tenancy::quota::{Exceeded, Held, Promise, QuotaEntry, QuotaOwner, shape};
use korpis_core::workload::Workload;
use std::collections::BTreeMap;
use uuid::Uuid;

/// The deepest an organization tree is walked; a cycle is impossible by schema, so reaching it
/// means corrupt data.
const MAX_DEPTH: usize = 64;

fn exceeded(e: &Exceeded) -> AppError {
    AppError::QuotaExceeded { dimension: e.dimension.as_str(), message: e.to_string() }
}

/// What `e`, an organization's entry, promised beneath it: the entries of its shape of the
/// organizations directly beneath its owner, from `beneath`, each less what it counted of the
/// workload being admitted, which it counts where its organization is on the workload's `chain`.
pub(crate) fn promises(
    e: &QuotaEntry,
    beneath: &[(Uuid, QuotaEntry)],
    (chain, labels): (&[Uuid], &BTreeMap<String, String>),
    counted: &Counted,
) -> Vec<Promise> {
    let QuotaOwner::Organization(org) = e.owner else { return Vec::new() };
    beneath
        .iter()
        .filter(|(parent, c)| *parent == org && shape(c) == shape(e))
        .map(|(_, c)| {
            // An entry not yet reckoned counts as what it was given until it is.
            let mut held = c.held.clone().unwrap_or_default();
            if counted.by.contains(&c.id) {
                held.sub(&counted.held);
            }
            let on_chain = matches!(c.owner, QuotaOwner::Organization(o) if chain.contains(&o));
            Promise { given: c.reservations.clone(), held, counts: on_chain && c.counts(labels) }
        })
        .collect()
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
        let chain = self.organization_chain(workload.organization).await?;
        let beneath = tx.promised_entries(&chain).await?;
        let counting: Vec<_> = entries.iter().filter(|e| e.counts(labels)).collect();
        for e in &counting {
            let mut rest = e.held.clone().unwrap_or_default();
            if counted.by.contains(&e.id) {
                rest.sub(&counted.held);
            }
            let promised = promises(e, &beneath, (&chain, labels), &counted);
            e.admit_promised(&rest, &promised, old, new).map_err(|x| exceeded(&x))?;
        }
        let ids: Vec<Uuid> = counting.iter().map(|e| e.id).collect();
        if !ids.is_empty() {
            tx.consume_quota(&ids).await?;
        }
        Ok(())
    }
}
