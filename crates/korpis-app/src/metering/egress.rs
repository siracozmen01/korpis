// SPDX-License-Identifier: AGPL-3.0-or-later

//! The egress budget, reckoned (§5.1 of docs/05-scheduling.md, "The egress budget"): every
//! minute each entry with a budget sums what the workloads it counts sent beyond their links this
//! month, as an invoice sums their records, and an entry whose sum reaches its budget is spent
//! until the month ends. Each workload a spent entry counts, where it is placed, is held to the
//! strictest of its spent entries' exhaustions; written apart from the entry, that hold announces
//! the workload, and its node is told in its next assignment (§4.3 of docs/02-architecture.md).

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, EgressAt, EgressWrite as _, RecipeSource, Store, Tx as _};
use korpis_core::tenancy::quota::{Exhaustion, QuotaEntry, QuotaOwner, Spent, month_of};
use std::collections::BTreeMap;
use std::time::SystemTime;
use uuid::Uuid;

/// What one pass reckoned: each entry's spend, and what each held workload is held to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reckoned {
    /// Each entry with a budget, and what it spent.
    pub spent: Vec<(Uuid, Spent)>,
    /// Each workload a spent entry counts where it is placed, and what it is held to.
    pub held: BTreeMap<Uuid, Exhaustion>,
}

/// Whether `entry` counts `at`: owned by its owner (`chain` the workload's organizations, its
/// own first), selected by its labels, and, for an entry that names nodes, on one of them.
fn counts(entry: &QuotaEntry, at: &EgressAt, chain: &[Uuid]) -> bool {
    let owned = match entry.owner {
        QuotaOwner::Organization(o) => chain.contains(&o),
        QuotaOwner::Project(p) => at.project == p,
    };
    owned && entry.counts(&at.labels) && (entry.nodes.is_none() || entry.counts_on(&at.node_labels))
}

/// What `entries` come to over `uses`, the month beginning at `month`, reckoned at `now`;
/// `chains` holds each workload's organizations.
#[must_use]
pub fn reckon(
    entries: &[QuotaEntry],
    uses: &[EgressAt],
    chains: &BTreeMap<Uuid, Vec<Uuid>>,
    (month, now): (SystemTime, SystemTime),
) -> Reckoned {
    let mut out = Reckoned::default();
    for entry in entries {
        let Some(budget) = entry.egress else { continue };
        let counted: Vec<&EgressAt> = uses
            .iter()
            .filter(|u| counts(entry, u, chains.get(&u.organization).map_or(&[], Vec::as_slice)))
            .collect();
        let bytes = counted.iter().fold(0_u64, |sum, u| sum.saturating_add(u.bytes));
        let exhausted = bytes >= budget.bytes_per_month;
        out.spent.push((entry.id, Spent { month, bytes, exhausted, reckoned_at: now }));
        if !exhausted {
            continue;
        }
        for u in counted.iter().filter(|u| u.placed) {
            let held = out.held.entry(u.workload).or_insert(budget.exhausted);
            *held = held.strictest(budget.exhausted);
        }
    }
    out
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Reckons every entry with an egress budget and holds the workloads of the spent ones; a
    /// workload no spent entry counts, or held last month, is let go. How many holds changed.
    pub async fn keep_egress(&self) -> Result<usize, AppError> {
        let now = self.clock.now();
        let (month, next) = month_of(now);
        let entries = self.store.budgeted_quota_entries().await?;
        let before = self.store.workload_egress_all().await?;
        if entries.is_empty() && before.is_empty() {
            return Ok(0);
        }
        let uses = self.store.egress_at(month, next).await?;
        let organizations: std::collections::BTreeSet<Uuid> =
            uses.iter().map(|u| u.organization).collect();
        let mut chains = BTreeMap::new();
        for o in organizations {
            chains.insert(o, self.organization_chain(o).await?);
        }
        let reckoned = reckon(&entries, &uses, &chains, (month, now));
        let mut tx = self.store.begin().await?;
        for (entry, spent) in &reckoned.spent {
            tx.put_quota_egress(*entry, spent).await?;
        }
        let mut changed = 0;
        for (workload, held) in &reckoned.held {
            if before.get(workload) != Some(&(month, *held)) {
                tx.put_workload_egress(*workload, month, *held).await?;
                changed += 1;
            }
        }
        for workload in before.keys().filter(|w| !reckoned.held.contains_key(w)) {
            tx.clear_workload_egress(*workload).await?;
            changed += 1;
        }
        tx.commit().await?;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests;
