// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis console` on a real node (§6 of docs/14-streams.md): an interactive workload runs in a
//! container under a terminal, and the `korpis` binary attaches to it with its input from a pipe.
//! What is typed reaches the workload, its answer comes back through the relay, and the input is
//! an effect of whoever typed it; a caller who may only watch is attached read-only.

use crate::node::{Node, live};
use crate::recipes::push_answerer;
use crate::support::{ControlPlane, converged, korpis};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// What `korpis console <w>` prints, as `secret`, with `typed` piped in, within `secs` seconds.
async fn attach(cp: &ControlPlane, secret: &str, w: &str, typed: &[u8], secs: u64) -> String {
    let mut child = tokio::process::Command::new(korpis())
        .envs(cp.cli_env(secret))
        .args(["console", w])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // Typed once the console is attached, as a person would.
    tokio::time::sleep(Duration::from_secs(2)).await;
    stdin.write_all(typed).await.unwrap();
    drop(stdin);
    let mut stdout = child.stdout.take().unwrap();
    let mut printed = Vec::new();
    let read = stdout.read_to_end(&mut printed);
    let _ = tokio::time::timeout(Duration::from_secs(secs), read).await;
    let _ = child.kill().await;
    String::from_utf8_lossy(&printed).into_owned()
}

#[tokio::test]
async fn korpis_console_types_into_an_interactive_workload_as_whoever_typed() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "fra-2").await;
    let agent = node.agent().await;
    let w = cp.declare(&project, "answerer", &push_answerer().await).await;
    node.guard(&w);
    converged(&cp, &w).await;

    let typist =
        cp.token_for(&["workload.read", "workload.console.read", "workload.console.write"]).await;
    let printed = attach(&cp, &typist, &w, b"hello\n", 20).await;
    assert!(printed.contains("ready"), "the scrollback: {printed:?}");
    assert!(printed.contains("got hello"), "the workload's answer: {printed:?}");
    let sql = "SELECT count(*) FROM effect WHERE action = 'workload.console.write' \
               AND actor::text LIKE '%token%' AND grant_id IS NOT NULL";
    assert_eq!(cp.scalar::<i64>(sql).await, 1, "the input, as the typist's act");

    let reader = cp.token_for(&["workload.read", "workload.console.read"]).await;
    let watched = attach(&cp, &reader, &w, b"ignored\n", 5).await;
    assert!(watched.contains("read-only"), "said so: {watched:?}");
    assert!(watched.contains("got hello"), "and shown the console: {watched:?}");
    assert!(!watched.contains("got ignored"), "typing nothing: {watched:?}");

    deleted_with_its_stream(&cp, &node, &w).await;
    agent.stop().await;
}

/// `korpis workload delete --delete-volumes stream_console`, approved by someone else: the
/// runtime goes, then the stream's volume, after its last segment reached the index, which then
/// holds that nothing of the stream is on the node.
async fn deleted_with_its_stream(cp: &ControlPlane, node: &Node, w: &str) {
    let proposer = cp.token_for(&["workload.read", "workload.delete", "volume.delete"]).await;
    let out = tokio::process::Command::new(korpis())
        .envs(cp.cli_env(&proposer))
        .args(["workload", "delete", w, "--delete-volumes", "stream_console"])
        .output()
        .await
        .unwrap();
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{printed} {}", String::from_utf8_lossy(&out.stderr));
    assert!(printed.contains("delete_volume") && printed.contains("IRREVERSIBLE"), "{printed}");
    let plan = printed.split_whitespace().find(|t| t.starts_with("pln_")).unwrap().to_owned();
    cp.approve(&plan).await;
    let deleted = "SELECT count(*) FROM volume WHERE kind = 'stream' AND deleted_at IS NOT NULL";
    for _ in 0..300 {
        if cp.scalar::<i64>(deleted).await == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(cp.scalar::<i64>(deleted).await, 1, "the stream's volume was deleted");
    let retired = "SELECT count(*) FROM effect WHERE action = 'workload.retire'";
    assert_eq!(cp.scalar::<i64>(retired).await, 1, "with the workload");
    let indexed = "SELECT count(*) FROM stream_segment";
    assert!(cp.scalar::<i64>(indexed).await >= 1, "its last segment reached the index");
    let held = "SELECT count(*) FROM stream_segment WHERE node_id IS NOT NULL";
    assert_eq!(cp.scalar::<i64>(held).await, 0, "and none is held on the node any more");
    let left: Vec<_> = std::fs::read_dir(&node.volumes).unwrap().filter_map(Result::ok).collect();
    assert!(left.is_empty(), "nothing is left on the node: {left:?}");
}
