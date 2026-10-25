// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Phase 0 sequence on a real node (§4 of docs/20-roadmap.md): a thin recipe is declared,
//! placed on the node, and converged, running in a hardened container with its configured value
//! and its volume; the agent restarts and adopts it without the workload starting again; every
//! step it took is an effect naming its grant; and its retirement removes the container and
//! keeps the volume. What it prints is its console stream, written beside it on the node, and the
//! segment sealed when its container goes is in the control plane's index (§4 of
//! docs/14-streams.md).

use crate::node::{Node, live};
use crate::recipes::push_sleeper;
use crate::support::ControlPlane;
use korpis_agent::runtime::{Runtime as _, RuntimeError};
use korpis_cli::cmd::Get;
use korpis_cli::cmd::get::get;
use korpis_cli::cmd::logs::{Logs, Reading, logs};
use korpis_cli::cmd::volume::{VolumeAction, volume};
use korpis_proto::korpis::v1 as pb;
use korpis_stream::dir::StreamDir;
use korpis_stream::read::read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Waits up to `secs` seconds until `pick` holds of the workload's status.
async fn status(
    cp: &ControlPlane,
    w: &str,
    secs: u64,
    pick: impl Fn(&pb::WorkloadStatus) -> bool,
) -> pb::WorkloadStatus {
    for _ in 0..secs * 5 {
        let s = cp.status(w).await;
        if pick(&s) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{w} never reached the status awaited: {:?}", cp.status(w).await);
}

fn converged(s: &pb::WorkloadStatus) -> bool {
    s.convergence.as_known() == Some(pb::Convergence::CONVERGENCE_CONVERGED)
}

/// How `korpis logs` reads with no flag given.
pub fn reading() -> Reading {
    Reading {
        follow: false,
        from: None,
        tail: None,
        since: None,
        grep: None,
        regex: false,
        limit: None,
        timestamps: false,
        relay: None,
        offloaded: false,
    }
}

/// Waits until `sql` counts at least `n`, for up to two minutes: a retirement stops a real
/// container, which its shim may take twelve seconds to kill, slower still beside every other
/// test.
async fn counted(cp: &ControlPlane, sql: &'static str, n: i64) {
    for _ in 0..600 {
        if cp.scalar::<i64>(sql).await >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let held = cp.scalar::<Option<String>>(PLANS).await;
    panic!("{sql} never reached {n}; the store held {held:?}");
}

/// Every workload's plans and the effects it was named in, said when a count never arrives.
const PLANS: &str = "SELECT 'plans ' || coalesce(
                       (SELECT string_agg(w.name || ' ' || p.kind || ' ' || p.status || ' '
                                          || p.computed_at::text, '; ' ORDER BY p.computed_at)
                          FROM plan p JOIN workload w ON w.id = p.workload_id), 'none')
                || ' | effects ' || coalesce(
                       (SELECT string_agg(w.name || ' ' || e.action || ' ' || e.outcome || ' '
                                          || e.at::text || ' ' || coalesce(e.error::text, ''),
                                          '; ' ORDER BY e.at)
                          FROM effect e JOIN workload w ON w.id = e.workload_id), 'none')";

/// The workload's one declared volume on the node, and its console stream's.
fn volumes(node: &Node) -> (PathBuf, PathBuf) {
    let dirs: Vec<PathBuf> = std::fs::read_dir(&node.volumes)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    let (console, declared): (Vec<_>, Vec<_>) =
        dirs.into_iter().partition(|p| p.to_string_lossy().ends_with(".stream_console"));
    assert_eq!((declared.len(), console.len()), (1, 1), "{declared:?} {console:?}");
    (declared[0].clone(), console[0].clone())
}

/// The lines of `file`, once it has at least one.
async fn lines(file: &Path) -> Vec<String> {
    for _ in 0..100 {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        if !text.trim().is_empty() {
            return text.lines().map(str::to_owned).collect();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{} stayed empty", file.display());
}

/// `korpis get volumes` lists the workload's volume and its console stream's, both detached
/// once it retired, as the node reported them.
async fn shown_detached(cp: &ControlPlane, tenant: &str, project: &str) {
    let asked = Get::Volumes { project: project.to_owned(), workload: None, deleted: false };
    let shown = get(&cp.cli(Some(tenant)), &asked).await.unwrap().human;
    let mut rows: Vec<Vec<&str>> =
        shown.lines().skip(1).map(|l| l.split_whitespace().collect()).collect();
    rows.sort_by_key(|r| r.get(1).copied());
    let picked: Vec<_> = rows.iter().map(|r| (r[1], r[2], r[6])).collect();
    assert_eq!(
        picked,
        [("data", "declared", "-"), ("stream_console", "stream", "-")],
        "both listed, attached to nothing: {shown}"
    );
}

/// That the workload whose volume is `vol` has started once; otherwise what the node did, as it
/// recorded it, so a start nothing should have made says why.
async fn started_once(cp: &ControlPlane, vol: &std::path::Path) {
    if lines(&vol.join("starts")).await.len() != 1 {
        let sql = "SELECT coalesce(string_agg(action || ' ' || outcome || ' ' ||
                    coalesce(after::text, ''), ' | ' ORDER BY at), '') FROM effect";
        panic!("never started again, and it was: {}", cp.scalar::<String>(sql).await);
    }
}

#[tokio::test]
async fn a_thin_recipe_runs_survives_an_agent_restart_untouched_and_retires() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "fra-1").await;
    let agent = node.agent().await;
    let w = cp.declare(&project, "sleeper", &push_sleeper().await).await;
    node.guard(&w);

    // Placed, fetched by digest, and running in its container.
    let s = status(&cp, &w, 180, converged).await;
    let observed = s.observation.as_option().unwrap().clone();
    assert_eq!(observed.state.as_known(), Some(pb::ObservedState::OBSERVED_STATE_RUNNING));
    let (vol, console) = volumes(&node);
    assert_eq!(lines(&vol.join("greeting")).await, ["hello"], "the configured value");
    assert_eq!(lines(&vol.join("starts")).await.len(), 1);
    let identity = lines(&vol.join("identity")).await;
    assert_eq!(identity, ["socket", "read-only"], "its identity socket, mounted read-only");
    let runtime = node.runtime();
    let pid = runtime.inspect(&observed.runtime_id).await.unwrap().pid;
    assert!(pid > 0);

    // Its console, read with `korpis logs` by a token that may read it and not its node.
    let tenant = cp.token_for(&["workload.read", "workload.console.read", "volume.read"]).await;
    let reading = Reading { follow: true, limit: Some(1), ..reading() };
    let asked = Logs { workload: w.clone(), reading };
    let mut printed = Vec::new();
    logs(&cp.cli(Some(&tenant)), &asked, &mut |line, _| printed.push(line)).await.unwrap();
    assert_eq!(printed, ["said hello"], "what it printed, read through a relay by its route");

    // The agent restarts; the workload does not.
    agent.stop().await;
    let agent = node.agent().await;
    counted(&cp, "SELECT count(*) FROM effect WHERE action = 'node.adopt'", 1).await;
    status(&cp, &w, 30, converged).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    started_once(&cp, &vol).await;
    assert_eq!(runtime.inspect(&observed.runtime_id).await.unwrap().pid, pid, "the same process");
    let sql = "SELECT count(*) FROM effect WHERE (action LIKE 'step.%' OR action = 'node.adopt') \
               AND origin_seq IS NOT NULL AND grant_id IS NULL";
    assert_eq!(cp.scalar::<i64>(sql).await, 0, "every node action names its grant");
    let sql = "SELECT count(*) FROM effect WHERE action IN ('workload.place', 'lease.issue') \
               AND grant_id IS NULL";
    assert_eq!(
        cp.scalar::<i64>(sql).await,
        0,
        "and so does every decision the control plane carried out"
    );

    // Retired: the container goes, the volume stays.
    cp.retire(&w).await;
    counted(&cp, "SELECT count(*) FROM effect WHERE action = 'workload.retire'", 1).await;
    let gone = runtime.inspect(&observed.runtime_id).await;
    assert!(matches!(gone, Err(RuntimeError::NotFound(_))), "{gone:?}");
    assert!(vol.join("starts").exists(), "a volume outlives its workload");
    shown_detached(&cp, &tenant, &project).await;

    // Its console: written on the node while it ran, sealed when its container went, indexed.
    let sql = "SELECT count(*) FROM stream_segment s JOIN stream ON stream.id = s.stream_id \
               WHERE stream.kind = 'console' AND s.end_offset = 1 AND s.digest LIKE 'sha256:%' \
               AND s.node_id IS NOT NULL";
    counted(&cp, sql, 1).await;
    let records = read(&StreamDir::new(&console), 0, 10).unwrap().records;
    let said: Vec<_> = records.iter().map(|r| String::from_utf8_lossy(&r.body)).collect();
    assert_eq!(said, ["said hello\n"], "what it printed, as it printed it");

    adopted(&cp, (&node, &project), &vol).await;
    retained_volume_deleted(&cp, &project, &vol).await;
    agent.stop().await;
}

/// The retained volume attached to a new workload, which finds what the first one wrote, in the
/// same directory on the same node; retired in turn, it keeps the volume again.
async fn adopted(cp: &ControlPlane, (node, project): (&Node, &str), vol: &Path) {
    let data = cp.volume_named(project, "data").await;
    let spec = pb::VolumeSpec { name: "data".into(), existing: Some(data), ..Default::default() };
    let repo = push_sleeper().await;
    let w = cp.declare_with(project, "adopter", &repo, vec![spec]).await;
    node.guard(&w);
    status(cp, &w, 180, converged).await;
    assert_eq!(lines(&vol.join("starts")).await.len(), 2, "its start is added to the first's");
    let dirs = std::fs::read_dir(&node.volumes).unwrap().filter_map(Result::ok).count();
    assert_eq!(dirs, 3, "no volume was made for it: the first's two, and its own console's");
    cp.retire(&w).await;
    let sql = "SELECT count(*) FROM effect WHERE action = 'workload.retire'";
    counted(cp, sql, 2).await;
    let sql = "SELECT count(*) FROM volume WHERE name = 'data' AND attached_to IS NULL";
    counted(cp, sql, 1).await;
}

/// The retained volume, its workload gone, deleted by a plan its node executes through a
/// residual assignment, which leaves the set once it is done.
async fn retained_volume_deleted(cp: &ControlPlane, project: &str, vol: &Path) {
    let data = cp.volume_named(project, "data").await;
    let proposer = cp.token_for(&["volume.read", "volume.delete"]).await;
    let asked = VolumeAction::Delete { volume: data };
    let answer = volume(&cp.cli(Some(&proposer)), &asked).await.unwrap();
    assert!(answer.human.contains("IRREVERSIBLE"), "{}", answer.human);
    let plan = answer.human.split_whitespace().find(|t| t.starts_with("pln_")).unwrap();
    cp.approve(plan).await;
    let sql = "SELECT count(*) FROM volume WHERE name = 'data' AND deleted_at IS NOT NULL";
    counted(cp, sql, 1).await;
    assert!(!vol.exists(), "its data is gone from the node");
    let sql = "SELECT count(*) FROM plan WHERE kind = 'delete_volume' AND status = 'applied'";
    counted(cp, sql, 1).await;
    for _ in 0..50 {
        if cp.scalar::<i64>("SELECT count(*) FROM assignment WHERE active").await == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("the residual assignment never left the set");
}
