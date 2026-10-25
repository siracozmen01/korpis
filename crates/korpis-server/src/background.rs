// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's own work, beside answering requests: placements retried, runs made,
//! silent nodes swept, changes dispatched, conditions tracked, certificates watched, offloads
//! expired, events pruned, and the rest, each a task that ends with the process.

use crate::ServeConfig;
use crate::api::Api;
use crate::{
    buckets, control_jobs, issuer, mesh, named_relays, provider_calls, reachability, records,
    revocations,
};
use korpis_store::PgStore;
use std::sync::Arc;
use std::time::Duration;

/// Starts the control plane's own work, each task ending with the process.
pub(crate) fn start(api: &Arc<Api>, config: &ServeConfig) {
    tokio::spawn(retry_placements(Arc::clone(api), config.placement_retry));
    tokio::spawn(make_runs(Arc::clone(api)));
    tokio::spawn(sweep_silent_nodes(Arc::clone(api), config.heartbeat_interval));
    tokio::spawn(dispatch_changes(Arc::clone(api)));
    tokio::spawn(track_conditions(Arc::clone(api)));
    tokio::spawn(provider_calls::keep_registrations(Arc::clone(api)));
    tokio::spawn(provider_calls::dispatch(Arc::clone(api)));
    tokio::spawn(revocations::keep(Arc::clone(api)));
    tokio::spawn(named_relays::keep(Arc::clone(api)));
    tokio::spawn(mesh::keep(Arc::clone(api)));
    tokio::spawn(alert_certificates(Arc::clone(api)));
    tokio::spawn(reachability::keep(Arc::clone(api)));
    tokio::spawn(close_denial_runs(Arc::clone(api), config.denial_window));
    tokio::spawn(expire_offloaded(Arc::clone(api)));
    tokio::spawn(reconcile_quota(Arc::clone(api)));
    tokio::spawn(keep_metering(Arc::clone(api)));
    tokio::spawn(reverify_bindings(Arc::clone(api)));
    tokio::spawn(forget_assertions(Arc::clone(api)));
    tokio::spawn(control_jobs::queue_steps(Arc::clone(api)));
    tokio::spawn(control_jobs::work(Arc::clone(api)));
    tokio::spawn(issuer::keep(Arc::clone(api)));
    tokio::spawn(records::keep(Arc::clone(api)));
    tokio::spawn(buckets::forget(Arc::clone(api)));
    tokio::spawn(buckets::forget_sessions(Arc::clone(api)));
}

/// Tries again, every `every`, each workload whose approved target no node could take: a node
/// enrolled, uncordoned, or freed is found here rather than waiting for the next approval. The
/// task ends with the process.
async fn retry_placements(api: Arc<Api>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().reconcile_placements().await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(tried = n, "placement retry"),
            Err(e) => tracing::warn!(error = %e, "placement retry failed"),
        }
    }
}

/// How often the runs of task and scheduled workloads are made: well within the minute an
/// occurrence's run may be late and still be on time (§4.3 of docs/05-scheduling.md).
const RUN_EVERY: Duration = Duration::from_secs(10);

/// Makes and ends the runs of task and scheduled workloads, every few seconds, as their
/// schedules, policies, and power say. The task ends with the process.
async fn make_runs(api: Arc<Api>) {
    let mut tick = tokio::time::interval(RUN_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().tend_runs().await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(workloads = n, "runs made or ended"),
            Err(e) => tracing::warn!(error = %e, "making runs failed"),
        }
    }
}

/// Marks unreachable, every heartbeat interval, each connected node not heard for three. The
/// task ends with the process.
async fn sweep_silent_nodes(api: Arc<Api>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().sweep_silent_nodes().await {
            Ok(0) => {}
            Ok(n) => tracing::info!(nodes = n, "nodes unreachable: three heartbeats missed"),
            Err(e) => tracing::warn!(error = %e, "node sweep failed"),
        }
    }
}

/// Writes, four times a window, the repeats of every run of denials whose window has closed,
/// so a run that stops repeating is in the log within about a window and a quarter of its last
/// repeat. The task ends with the process.
async fn close_denial_runs(api: Arc<Api>, window: Duration) {
    let mut tick = tokio::time::interval((window / 4).max(Duration::from_millis(250)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().close_denial_runs().await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(runs = n, "denial runs closed"),
            Err(e) => tracing::warn!(error = %e, "closing denial runs failed"),
        }
    }
}

/// Decides workloads' convergence on every announced change and every resync, writing an event
/// when it changed (§5 of docs/15-observability.md). The task ends with the process.
async fn track_conditions(api: Arc<Api>) {
    use korpis_app::ports::{Change, Changes as _, Store as _};
    let mut changes = api.app().store().subscribe();
    let mut next = Some(Change::Resync);
    while let Some(change) = next {
        let decided = match change {
            Change::Workload(w) => api.app().track_convergence(w).await.map(u64::from),
            Change::Resync => api.app().track_all_convergence().await,
        };
        if let Err(e) = decided {
            tracing::warn!(error = %e, "deciding convergence failed");
        }
        next = changes.next().await;
    }
}

/// Reports, hourly, each relay certificate and each ingress certificate that came a step closer to
/// expiring (§7 of docs/18-operations.md, §3.2 of docs/07-networking.md). The task ends with the
/// process.
async fn alert_certificates(api: Arc<Api>) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().alert_relay_certificates().await {
            Ok(0) => {}
            Ok(n) => tracing::warn!(relays = n, "relay certificates nearing expiry"),
            Err(e) => tracing::warn!(error = %e, "checking relay certificates failed"),
        }
        match api.app().alert_certificates().await {
            Ok(0) => {}
            Ok(n) => tracing::warn!(certificates = n, "ingress certificates nearing expiry"),
            Err(e) => tracing::warn!(error = %e, "checking ingress certificates failed"),
        }
    }
}

/// Deletes, hourly, offloaded stream segments past their retention (§4 of docs/14-streams.md).
/// The task ends with the process.
async fn expire_offloaded(api: Arc<Api>) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().expire_offloaded().await {
            Ok(pass) => {
                if pass.deleted > 0 {
                    tracing::info!(segments = pass.deleted, "offloaded segments past retention");
                }
                for (repository, workload, e) in pass.failed {
                    tracing::warn!(%repository, %workload, error = %e,
                        "offloaded segments past their retention were not deleted");
                }
            }
            Err(e) => tracing::warn!(error = %e, "expiring offloaded segments failed"),
        }
    }
}

/// How often due identity bindings are looked for: well within the shortest interval one may
/// declare, a minute (§5.1 of docs/08-identity.md).
const REVERIFY_EVERY: Duration = Duration::from_secs(5);

/// Re-verifies, every few seconds, the identity bindings due, so a membership that ended stops
/// authorizing within its binding's interval. The task ends with the process.
async fn reverify_bindings(api: Arc<Api>) {
    let mut tick = tokio::time::interval(REVERIFY_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().reverify_due().await {
            Ok((0, None)) => {}
            Ok((n, None)) => tracing::debug!(bindings = n, "identity bindings re-verified"),
            Ok((n, Some(e))) => {
                tracing::warn!(bindings = n, error = %e, "recording a re-verification failed");
            }
            Err(e) => tracing::warn!(error = %e, "re-verifying identity bindings failed"),
        }
    }
}

/// Forgets, every minute, the presentations and tickets past use (§5.2 of
/// docs/08-identity.md), and the sign-ins begun that expired unspent (§9.7). The task ends with
/// the process.
async fn forget_assertions(api: Arc<Api>) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Err(e) = api.app().forget_assertions().await {
            tracing::warn!(error = %e, "forgetting presentations, tickets, and sign-ins failed");
        }
    }
}

/// Recounts, hourly, what every workload holds against quota and checks every entry's totals
/// against it (question 7 of §10 of docs/05-scheduling.md); a difference is corrected and
/// recorded as a `quota.drift` effect. The first pass is the one at start. The task ends with the
/// process.
async fn reconcile_quota(api: Arc<Api>) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        tick.tick().await;
        match api.app().reconcile_quota().await {
            Ok(0) => {}
            Ok(n) => tracing::warn!(corrected = n, "quota totals drifted and were corrected"),
            Err(e) => tracing::warn!(error = %e, "reconciling quota failed"),
        }
    }
}

/// Writes, every minute, the metering gaps now due and closes the days whose lateness window has
/// passed (§2 of docs/15-observability.md), then reckons the egress budgets (§5.1 of
/// docs/05-scheduling.md). The task ends with the process.
async fn keep_metering(api: Arc<Api>) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().keep_metering().await {
            Ok(k) if k.gaps > 0 || k.closed > 0 => {
                tracing::info!(gaps = k.gaps, closed = k.closed, "metering kept");
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "keeping metering failed"),
        }
        match api.app().keep_egress().await {
            Ok(0) => {}
            Ok(n) => tracing::info!(changed = n, "egress budgets reckoned"),
            Err(e) => tracing::warn!(error = %e, "reckoning egress budgets failed"),
        }
    }
}

/// Prunes the event log hourly to its retention window. The task ends with the process.
pub(crate) async fn prune_events(store: PgStore, retention: Duration) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match store.prune_events(retention).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(events = n, "events past retention pruned"),
            Err(e) => tracing::warn!(error = %e, "pruning the event log failed"),
        }
    }
}

/// Wakes the session of every node a workload change concerns, so its set is recomputed and,
/// if it changed, sent; a resync wakes them all. The task ends with the process.
async fn dispatch_changes(api: Arc<Api>) {
    use korpis_app::ports::{Change, Changes as _, Store as _};
    let mut changes = api.app().store().subscribe();
    while let Some(change) = changes.next().await {
        match change {
            Change::Workload(w) => match api.app().nodes_concerned(w).await {
                Ok(nodes) => nodes.into_iter().for_each(|n| api.sessions().wake(n)),
                Err(e) => {
                    tracing::warn!(error = %e, "change dispatch failed; waking every session");
                    api.sessions().wake_all();
                }
            },
            Change::Resync => api.sessions().wake_all(),
        }
    }
}
