// SPDX-License-Identifier: AGPL-3.0-or-later

//! A task's run on a real node (§4 of docs/05-scheduling.md, §4 of docs/14-streams.md): its
//! container starts once, prints, and exits by itself; the control plane judges it succeeded
//! from the exit status its node reports; and what it printed is a console of its own, read by
//! naming the run with `korpis run logs`. A run that keeps running is not judged lost while its
//! node holds the workload's lease (§4.2 there).

use crate::node::{Node, live};
use crate::oci::reading;
use crate::recipes::{push_printer, push_waiter};
use crate::support::ControlPlane;
use std::time::Duration;

/// The task's one run, once it has ended: its identifier and status.
async fn ended(cp: &ControlPlane) -> (String, String) {
    let sql = "SELECT (SELECT id::text || ' ' || status FROM run WHERE ended_at IS NOT NULL)";
    for _ in 0..900 {
        if let Some(row) = cp.scalar::<Option<String>>(sql).await
            && let Some((id, status)) = row.split_once(' ')
        {
            let id =
                korpis_core::id::Id::from_uuid(korpis_core::id::IdKind::Run, id.parse().unwrap());
            return (id.to_string(), status.to_owned());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("the task's run never ended");
}

#[tokio::test]
async fn a_task_s_run_prints_ends_by_itself_and_its_console_is_read_by_naming_it() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "fra-1").await;
    let _agent = node.agent().await;
    let w = cp.declare(&project, "printer", &push_printer().await).await;
    node.guard(&w);

    let (run, status) = ended(&cp).await;
    let why = "SELECT (SELECT coalesce(cause, '') || ' ' || coalesce(exit_status::text, '-')
                         || ' ' || coalesce((SELECT after::text || ' ' || coalesce(error::text, '')
                                               FROM effect WHERE action = 'run.ended'), '')
                  FROM run WHERE ended_at IS NOT NULL)";
    let why = cp.scalar::<Option<String>>(why).await;
    assert_eq!(status, "succeeded", "judged by its exit status, zero: {why:?}");

    let tenant = cp.token_for(&["workload.read", "workload.console.read"]).await;
    let mut printed = Vec::new();
    let c = cp.cli(Some(&tenant));
    korpis_cli::cmd::run::logs(&c, &run, &reading(), &mut |line, _| printed.push(line))
        .await
        .unwrap();
    assert_eq!(printed, ["backing up", "3 files", "done"], "its own console, from its node");
}

/// The runs and the leases, said when a run was judged lost.
const SEEN: &str = "SELECT (SELECT string_agg(w.name || ' ' || r.status || ' '
                                   || coalesce(r.cause, '-'), '; ')
                      FROM run r JOIN workload w ON w.id = r.workload_id)
               || ' | leases ' || coalesce(
                      (SELECT string_agg(w.name || ' until ' || l.grantor_expiry::text
                                         || ' released ' || coalesce(l.released_at::text, '-'),
                                         '; ')
                         FROM lease l JOIN workload w ON w.id = l.workload_id), 'none')
               || ' | now ' || now()::text";

#[tokio::test]
async fn a_task_s_run_that_keeps_running_is_not_judged_lost_while_its_node_holds_the_lease() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "fra-2").await;
    let _agent = node.agent().await;
    let w = cp.declare(&project, "waiter", &push_waiter().await).await;
    node.guard(&w);
    let p = cp.declare(&project, "printer", &push_printer().await).await;
    node.guard(&p);

    let running = "SELECT count(*) FROM run r JOIN workload w ON w.id = r.workload_id
                    WHERE w.name = 'waiter' AND r.status = 'running'";
    for _ in 0..900 {
        if cp.scalar::<i64>(running).await == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // Several of the run maker's passes, each of which would end a run whose node it held lost.
    tokio::time::sleep(Duration::from_secs(20)).await;
    let said = cp.scalar::<Option<String>>(SEEN).await;
    assert_eq!(cp.scalar::<i64>(running).await, 1, "still running: {said:?}");
}
