// SPDX-License-Identifier: AGPL-3.0-or-later

//! A control plane restored to an older backup while its agent runs, end to end (§8 of
//! docs/03-state.md, §6 of docs/18-operations.md): backed up, changed, stopped, restored from the
//! older database and the backup file by `restore`, and started again. Before it serves anyone,
//! the agent presents what it holds: the workload the backup never knew is recovered where it
//! runs, and the intent it never held is recovered with the plan applying it; once the window
//! ends the control plane serves what the node runs.

use crate::session::until;
use crate::support::{ControlPlane, facts, mutation, push_paper};
use korpis_agent::held::HeldSet;
use korpis_agent::join::{Join, join};
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::session::{Event, Link, Session};
use korpis_agent::state::{Identity, StateDir};
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use korpis_server::control_backup::{backup, declared};
use korpis_server::install::RecoverConfig;
use korpis_server::install::target::{BackupTarget, declare};
use korpis_server::restore::{RestoreConfig, restore};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::Receiver;

/// The agent: what it keeps on disk, and its running session.
struct Agent {
    identity: Identity,
    state: StateDir,
    journal: JournalHandle,
    running: Option<tokio::task::JoinHandle<()>>,
}

impl Agent {
    /// A session dialled now, as the agent's own reconnection would, once it is welcomed; what
    /// it says after that.
    async fn connect(&mut self) -> (Link, Receiver<Event>) {
        if let Some(previous) = self.running.take() {
            previous.abort();
        }
        let (session, link) = Session::new(
            self.identity.clone(),
            self.state.clone(),
            Arc::new(facts),
            self.journal.clone(),
        );
        let mut events = link.events.subscribe();
        self.running = Some(tokio::spawn(session.run()));
        until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
        (link, events)
    }
}

async fn holds(link: &Link, workloads: &[&str]) {
    let mut held = link.held.clone();
    let all = |h: &HeldSet| workloads.iter().all(|w| h.assignments.contains_key(*w));
    let waited = tokio::time::timeout(Duration::from_secs(30), held.wait_for(all)).await;
    waited.expect("within thirty seconds").expect("the session holds a set");
}

/// Waits until the control plane has recorded `n` effects of `action`, and no more.
async fn effects(cp: &ControlPlane, action: &str, n: i64) {
    let mut seen = None;
    for _ in 0..200 {
        seen = cp.query_one("SELECT count(*) FROM effect WHERE action = $1", action).await;
        if seen == Some((n,)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{action}: {n} awaited, {seen:?} recorded");
}

/// A new intent for `workload`, approved: a memory reservation it did not have. The intent, and
/// its plan.
async fn change(cp: &ControlPlane, workload: &str) -> (String, String) {
    let body = pb::IntentBody {
        recipe: pb::RecipeRef {
            name: push_paper().await,
            version: "1.21.4".into(),
            ..Default::default()
        }
        .into(),
        resources: pb::ResourceSpec {
            memory: pb::ReservedLimit { reservation: 3 << 30, ..Default::default() }.into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    };
    let request = pb::DeclareIntentRequest {
        mutation: pb::Mutation { expected_version: Some(1), ..mutation("change-w") }.into(),
        target: Some(Target::Workload(workload.to_owned())),
        body: body.into(),
        ..Default::default()
    };
    let declared = cp.workloads().declare_intent(request).await.unwrap().into_owned();
    let plan = declared.plan.into_option().unwrap();
    if plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
        let approve = pb::ApprovePlanRequest {
            mutation: mutation("approve-change-w").into(),
            plan: plan.id.clone(),
            ..Default::default()
        };
        cp.client(pb::PlanServiceClient::new).approve_plan(approve).await.unwrap();
    }
    (declared.intent.into_option().unwrap().id, plan.id)
}

/// What the agent's reconciler journals once it has applied the plan `workload` is applying:
/// every step done. Returns once the control plane has it applied.
async fn applied(cp: &ControlPlane, journal: &JournalHandle, workload: &str) {
    let sql = "SELECT id FROM plan WHERE workload_id::text = $1 AND status = 'applying'";
    let mut applying = None;
    for _ in 0..100 {
        applying = cp.query_one::<(uuid::Uuid,)>(sql, &uuid(workload)).await;
        if applying.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (plan,) = applying.expect("a plan applying");
    let plan = Id::from_uuid(IdKind::Plan, plan).to_string();
    for step in cp.plan(&plan).await.steps {
        let effect = pb::Effect {
            workload: Some(workload.to_owned()),
            plan: Some(plan.clone()),
            step: Some(step.index),
            action: "plan.step".into(),
            outcome: pb::EffectOutcome::EFFECT_OUTCOME_SUCCEEDED.into(),
            ..Default::default()
        };
        journal.append(Body::Effect(Box::new(effect))).await.unwrap();
    }
    for _ in 0..100 {
        if cp.plan(&plan).await.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_APPLIED) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{plan} was not applied");
}

/// The backup, as an operator takes it: `korpis-server backup` into the target installation
/// declared, in `dir`, then the database copied once serving has stopped, as PostgreSQL's own
/// backup of it would restore. The database, and the file.
async fn back_up(
    cp: &mut ControlPlane,
    dir: &std::path::Path,
) -> ((korpis_store::testing::TestDb, String), std::path::PathBuf) {
    let target = dir.join("target");
    std::fs::create_dir(&target).unwrap();
    declare(&cp.data, &BackupTarget::Directory(target)).unwrap();
    let done = backup(&cp.data, cp.login(), 3).await.unwrap();
    cp.stop();
    (cp.snapshot().await, done.path)
}

/// The agent, joined to `cp` and connected.
async fn joined(cp: &ControlPlane, dir: &std::path::Path) -> (Agent, Link) {
    let (token, ca) = cp.token("fra-8").await;
    let state = StateDir::new(dir.join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    let identity = join(&state, &request).await.unwrap();
    let journal = JournalHandle::open(&state.root().join("journal"), Bounds::default()).unwrap();
    let mut agent = Agent { identity, state, journal, running: None };
    let (link, _) = agent.connect().await;
    (agent, link)
}

/// After the backup: workload `x`, which the backup never knew, and a new intent for `w`, which
/// it never held, both reaching the node. `x`, the new intent of `w`, and its plan.
async fn after_backup(cp: &ControlPlane, link: &Link, project: &str, w: &str) -> [String; 3] {
    let x = cp.declare(project, "x").await;
    let (changed, plan) = change(cp, w).await;
    holds(link, &[w, &x]).await;
    let mut held = link.held.clone();
    let runs = |h: &HeldSet| h.assignments[w].target.as_option().is_some_and(|t| t.id == changed);
    tokio::time::timeout(Duration::from_secs(30), held.wait_for(runs)).await.unwrap().unwrap();
    [x, changed, plan]
}

/// What the agent's presentation recovers while the window is open, and what the restore did
/// before: every count exact, and nothing held, since the node presented everything placed on it.
async fn recovered(cp: &ControlPlane) {
    for (action, n) in [
        ("restore.epochs.fenced", 1),
        ("restore.token_key.rotated", 1),
        ("restore.relays.held", 1),
        ("restore.gap", 1),
        ("restore.settling.opened", 1),
        // x, created after the backup: the workload, its intent, power, placement and slot.
        ("restore.workload.recovered", 1),
        ("restore.power.recovered", 1),
        ("restore.placement.recovered", 1),
        ("restore.id_slot.recovered", 1),
        // x's intent and w's new one, each with its counters and the plan applying it.
        ("restore.intent.recovered", 2),
        ("restore.targets.recovered", 2),
        ("restore.plan.recovered", 2),
        ("restore.placement.held", 0),
    ] {
        effects(cp, action, n).await;
    }
}

#[tokio::test]
async fn a_control_plane_restored_to_an_older_backup_recovers_what_its_agent_runs_first() {
    let mut cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let (mut agent, link) = joined(&cp, dir.path()).await;
    let w = cp.declare(&project, "w").await;
    holds(&link, &[&w]).await;
    applied(&cp, &agent.journal, &w).await;
    let (database, file) = back_up(&mut cp, dir.path()).await;

    cp.serve_on(None).await;
    let (link, _) = agent.connect().await;
    let [x, changed, plan] = after_backup(&cp, &link, &project, &w).await;

    // The disk fails. The older database is restored, then the file, with the agent running.
    cp.stop();
    let config = RestoreConfig {
        recover: RecoverConfig { data: cp.data.clone(), database_url: database.1.clone() },
        from: file,
        key: declared(&cp.data).unwrap().1,
        settle: Duration::from_secs(6),
    };
    let restored = restore(&config, "uid 0 (root)").await.unwrap();
    assert_eq!(restored.placed.written, 0, "the data directory kept the backup's keys");
    cp.serve_on(Some(database)).await;
    let (link, mut events) = agent.connect().await;

    // Nobody is served while it settles, and the agent's presentation is recovered.
    let get = pb::GetWorkloadRequest { id: w.clone(), ..Default::default() };
    let refused = cp.workloads().get_workload(get).await.unwrap_err();
    assert_eq!(refused.code, connectrpc::ErrorCode::Unavailable, "{refused:?}");
    recovered(&cp).await;

    // Once the window ends, callers are served what the node runs: x placed where it runs, not
    // placed again; w's new intent its head and target, its plan applying. The set the restored
    // control plane answers the node with, the one it held or a new one, names both.
    tokio::time::sleep(Duration::from_secs(7)).await;
    let p = cp.status(&x).await.placement.into_option().expect("x placed");
    assert_eq!(p.bound_by.as_known(), Some(pb::BoundBy::BOUND_BY_RECOVERY));
    let s = cp.status(&w).await;
    assert_eq!(s.head.as_option().map(|i| i.id.as_str()), Some(changed.as_str()));
    assert_eq!(s.target.as_option().map(|i| i.id.as_str()), Some(changed.as_str()));
    let applying = cp.plan(&plan).await.status.as_known();
    assert_eq!(applying, Some(pb::PlanStatus::PLAN_STATUS_APPLYING));
    until(&mut events, |e| matches!(e, Event::Held { .. } | Event::InSync)).await;
    holds(&link, &[&w, &x]).await;
}

fn uuid(wire: &str) -> String {
    Id::parse(IdKind::Workload, wire).unwrap().uuid().to_string()
}
