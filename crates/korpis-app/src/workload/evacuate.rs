// SPDX-License-Identifier: AGPL-3.0-or-later

//! Evacuating a workload off a node that cannot take part: placed again from its target as
//! failover places one, but on an operator's decision, which accepts what failover never does on
//! its own. Each volume comes from where another node reaches it, or else from its freshest
//! replica, losing what was written since; or else, its only copy lost with the node, it is made
//! anew holding its latest backup, losing what was written since, or empty, losing everything.
//! A `keep_running` workload may still be running on the node, and a copy started elsewhere splits
//! it; the evacuation says so before it is approved. A `stop` one still waits for its lease, since
//! it stops by its own clock once that ends and two writers would corrupt a volume both reach.

use super::failover::{Evacuated, Failover};
use super::migrate::For;
use super::screen::Screened;
use crate::App;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{
    Clock, LeaseWrite as _, PlacementWrite as _, RecipeSource, Store, VolumeWrite as _,
    WorkloadWrite as _,
};
use korpis_core::backup::BackupStatus;
use korpis_core::workload::spec::Continuity;
use korpis_core::workload::volume::Home;
use serde_json::{Map, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Where an evacuated workload's volume comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// Where it is, which another node reaches: nothing is lost.
    Reached,
    /// Its freshest replica, which received its last snapshot then.
    Replica(SystemTime),
    /// A backup, finished then.
    Backup(Uuid, SystemTime),
    /// Nothing: it is made empty.
    Empty,
}

/// What evacuating a workload does with its data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Evacuation {
    /// Each volume it holds, by name, with its record and where it comes from.
    pub(crate) volumes: Vec<(String, Uuid, Source)>,
    /// Whether it may still be running on the node.
    pub(crate) may_still_run: bool,
}

/// Seconds since the epoch, as a detail.
fn seconds(t: SystemTime) -> Value {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()).into()
}

impl Evacuation {
    /// What it does, as an item's detail: per volume where it comes from and since when, and
    /// whether the workload may still be running.
    pub(crate) fn detail(&self) -> Map<String, Value> {
        let volumes: Vec<Value> = self
            .volumes
            .iter()
            .map(|(name, _, source)| {
                let mut v = Map::from_iter([("name".to_owned(), name.clone().into())]);
                let (from, since) = match source {
                    Source::Reached => ("where_it_is", None),
                    Source::Replica(at) => ("replica", Some(*at)),
                    Source::Backup(id, at) => {
                        v.insert("backup".to_owned(), id.to_string().into());
                        ("backup", Some(*at))
                    }
                    Source::Empty => ("empty", None),
                };
                v.insert("from".to_owned(), from.into());
                if let Some(at) = since {
                    v.insert("lost_since".to_owned(), seconds(at));
                }
                Value::Object(v)
            })
            .collect();
        Map::from_iter([
            ("volumes".to_owned(), Value::Array(volumes)),
            ("may_still_run".to_owned(), self.may_still_run.into()),
        ])
    }
}

/// What evacuating a workload did.
pub(crate) enum Evacuating {
    /// It is not on the node any more.
    Gone,
    /// Its lease has not ended yet; it is tried again.
    NotYet,
    /// It was placed again, made by this remake plan.
    Placed(Option<Uuid>),
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// What evacuating workload `id` off node `gone` would do with its data now.
    pub(crate) async fn evacuation(&self, id: Uuid, gone: Uuid) -> Result<Evacuation, AppError> {
        let Some(w) = self.store.workload(id).await? else { return Ok(Evacuation::default()) };
        let Some(t) = w.target else { return Ok(Evacuation::default()) };
        let Some(target) = self.store.intent(t).await? else {
            return Ok(Evacuation::default());
        };
        let held = self.held_volumes(&w, (&target.body, Some(gone)), (true, &[])).await?;
        let mut volumes = Vec::with_capacity(held.len());
        for h in held {
            let source = match (&h.home, &h.promote) {
                (_, Some(p)) => Source::Replica(p.received_at),
                (Home::Node(n), None) if *n == gone => self.latest_backup(id, h.id).await?,
                _ => Source::Reached,
            };
            volumes.push((h.name, h.id, source));
        }
        let may_still_run = self.on_expiry(gone, &target.body).await? != Continuity::Stop;
        Ok(Evacuation { volumes, may_still_run })
    }

    /// The newest whole backup of `volume`, of workload `w`, or nothing.
    async fn latest_backup(&self, w: Uuid, volume: Uuid) -> Result<Source, AppError> {
        let mut page = Page::new(None, Page::DEFAULT);
        loop {
            let found = self.store.backups(w, false, page).await?;
            let whole =
                found.iter().find(|b| b.volume == volume && b.status == BackupStatus::Succeeded);
            if let Some(b) = whole {
                return Ok(Source::Backup(b.id, b.created));
            }
            match page.next(&found, |b| b.id) {
                Some(after) => page = Page::new(Some(after), page.size),
                None => return Ok(Source::Empty),
            }
        }
    }
    /// Evacuates workload `id`, locked here, off node `gone` for the operation `operation`, by
    /// `approver` under `grant`: each volume lost with the node left behind and made anew where it
    /// lands, holding its latest backup if it has one, and the workload placed again and made by a
    /// remake plan. One with a plan under way on the node is refused, since the node holds what
    /// that plan made; one no node can take is refused, and nothing it did is kept.
    pub(crate) async fn evacuate_for(
        &self,
        tx: &mut S::Tx,
        (operation, approver, grant): For<'_>,
        (id, gone): (Uuid, Uuid),
    ) -> Result<Evacuating, AppError> {
        let now = self.clock.now();
        let Some(w) = tx.lock_workload(id).await?.filter(|w| w.deleted_at.is_none()) else {
            return Ok(Evacuating::Gone);
        };
        let Some(t) = w.target else { return Ok(Evacuating::Gone) };
        let Some(target) = tx.intent_within(t).await? else { return Ok(Evacuating::Gone) };
        let lease = tx.lease_for_update(id).await?;
        tx.lock_placement().await?;
        if tx.placement_in(id).await?.is_none_or(|p| p.node != gone) {
            return Ok(Evacuating::Gone);
        }
        if !tx.made_and_settled(id).await? {
            let why = "a plan of it is under way on the node; cancel it, and it is evacuated";
            return Err(AppError::conflict("workload.under_way", why));
        }
        let ev = self.evacuation(id, gone).await?;
        if !ev.may_still_run && lease.is_some_and(|l| !l.over(now)) {
            return Ok(Evacuating::NotYet);
        }
        let (mut lost, mut seeded) = (Vec::new(), Vec::new());
        for (name, record, source) in &ev.volumes {
            let attached =
                target.body.volumes.iter().any(|v| v.name == *name && v.existing.is_some());
            match source {
                Source::Reached | Source::Replica(_) => continue,
                _ if attached => {
                    let why = format!(
                        "its declaration attaches volume {name}, lost with the node; \
                                       declare it again without it"
                    );
                    return Err(AppError::conflict("volume.lost", why));
                }
                Source::Backup(backup, _) => seeded.push((name.clone(), *backup)),
                Source::Empty => {}
            }
            tx.leave_volume(*record, id, now).await?;
            lost.push(*record);
        }
        let evacuated =
            Evacuated { operation, by: approver.principal.clone(), grant, lost, seeded };
        let failover = Failover { from: gone, evacuated: Some(evacuated) };
        let screened = Screened::default();
        let plan = self.place_new(tx, (&w, &target), None, &screened, Some(&failover)).await?;
        if tx.placement_in(id).await?.is_none_or(|p| p.node == gone) {
            let why = "no node can run it now; its explanation says why";
            return Err(AppError::Unsatisfiable {
                reason: "placement.unsatisfiable",
                message: why.into(),
            });
        }
        self.count_placed(tx, &w).await?;
        Ok(Evacuating::Placed(plan))
    }
}
