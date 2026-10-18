// SPDX-License-Identifier: AGPL-3.0-or-later

//! A task's run on a real node (§4 of docs/05-scheduling.md, §4 of docs/14-streams.md): its
//! container starts once, prints, and exits by itself; the control plane judges it succeeded
//! from the exit status its node reports; and what it printed is a console of its own, read by
//! naming the run with `korpis run logs`.

use crate::node::{Node, live};
use crate::oci::reading;
use crate::recipes::push_printer;
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
