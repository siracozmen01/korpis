// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading what workloads used (§2 of docs/15-observability.md): of one workload, a project's,
//! or an organization's subtree's, under `metering.read` over what is named (§5 of
//! docs/10-api.md), and the days of metering with where each stands. A record's node is shown
//! only to a caller who may read that node, as an effect's is (§4 there).

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::{Page, Paged};
use crate::ports::{Clock, RecipeSource, Store, UsageQuery};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::metering::{DAY, Period, Record, day_of};
use std::collections::BTreeSet;
use std::time::SystemTime;
use uuid::Uuid;

/// Whose usage to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageOf {
    /// One workload's.
    Workload(Uuid),
    /// Every workload of a project.
    Project(Uuid),
    /// Every workload of an organization's subtree.
    Organization(Uuid),
}

/// The intervals to read: those beginning from `from` and before `to`, and whether superseded
/// records are read too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageWindow {
    /// From.
    pub from: SystemTime,
    /// To.
    pub to: SystemTime,
    /// Every version, superseded ones included.
    pub every_version: bool,
}

/// Where a page of usage resumes: after this interval start and record.
pub type UsageCursor = (SystemTime, Uuid);

/// The most days one listing of periods covers.
const MOST_DAYS: u32 = 366;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// What `of` used over `window`, a page at a time.
    pub async fn list_usage(
        &self,
        caller: &Caller,
        of: UsageOf,
        window: UsageWindow,
        page: Page<UsageCursor>,
    ) -> Result<Paged<Record, UsageCursor>, AppError> {
        let (object, query) = match of {
            UsageOf::Workload(id) => (Kind::Workload, (Some(id), None, None)),
            UsageOf::Project(id) => (Kind::Project, (None, Some(id), None)),
            UsageOf::Organization(id) => (Kind::Organization, (None, None, Some(id))),
        };
        let id = query.0.or(query.1).or(query.2).unwrap_or_default();
        self.authorize(caller, "metering.read", ObjectRef::new(object, id)).await?;
        let (workload, project, organization) = query;
        let q = UsageQuery {
            workload,
            project,
            organization,
            from: window.from,
            to: window.to,
            current: !window.every_version,
            after: page.after,
            limit: page.size,
        };
        let mut items = self.store.metering_usage(&q).await?;
        let nodes: BTreeSet<Uuid> = items.iter().filter_map(|r| r.node).collect();
        let asked: Vec<(&str, ObjectRef)> =
            nodes.iter().map(|n| ("node.read", ObjectRef::new(Kind::Node, *n))).collect();
        let permitted = self.permitted_all(caller, &asked).await?;
        let readable: BTreeSet<Uuid> =
            nodes.iter().zip(permitted).filter_map(|(n, p)| p.then_some(*n)).collect();
        for r in &mut items {
            r.node = r.node.filter(|n| readable.contains(n));
        }
        let next = page.next(&items, |r| (r.from, r.id));
        Ok(Paged { items, next })
    }

    /// The days of metering beginning from `from` and before `to`, at most a year of them, each
    /// with where it stands now.
    pub async fn metering_periods(
        &self,
        from: SystemTime,
        to: SystemTime,
    ) -> Result<Vec<Period>, AppError> {
        let first = day_of(from);
        let last = to.min(first + DAY * MOST_DAYS);
        let closed = self.store.metering_closed_days(first, last).await?;
        let now = self.clock.now();
        let mut out = Vec::new();
        let mut day = first;
        while day < last {
            let at = closed.iter().find(|(d, _)| *d == day).map(|(_, at)| *at);
            out.push(Period::of(day, at, now, self.metering_lateness));
            day += DAY;
        }
        Ok(out)
    }
}
