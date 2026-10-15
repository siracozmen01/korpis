// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dependencies on a real node (§5 of docs/07-networking.md): a web application that depends on
//! a database is placed beside it, created once the database runs, told its address in its
//! environment and in a file, and is let through to it by the node, where a workload that
//! declares no dependency on it is not.

use crate::node::{Node, live};
use crate::recipes::push_server;
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::dependency::Target;
use std::process::Command;
use std::time::Duration;

/// The address the node gave `w`.
fn address_of(node: &Node, w: &str) -> Option<String> {
    let kept = std::fs::read_to_string(node.state.root().join("addresses")).unwrap_or_default();
    kept.lines().find_map(|l| l.strip_prefix(&format!("{w} ")).map(str::to_owned))
}

/// The process id of `w`'s running task, once it runs.
async fn running(w: &str) -> u32 {
    for _ in 0..900 {
        let ctr = |args: &[&str]| Command::new("ctr").args(["-n", "korpis"]).args(args).output();
        let filter = format!("labels.\"korpis.workload\"=={w}");
        let ids = ctr(&["containers", "ls", "-q", &filter]).unwrap();
        let id = String::from_utf8_lossy(&ids.stdout).split_whitespace().next().map(str::to_owned);
        let tasks = ctr(&["tasks", "ls"]).unwrap();
        let listed = String::from_utf8_lossy(&tasks.stdout).into_owned();
        let pid = id.and_then(|id| {
            let line = listed.lines().find(|l| l.starts_with(&id) && l.contains("RUNNING"))?;
            line.split_whitespace().nth(1)?.parse().ok()
        });
        if let Some(pid) = pid {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{w} never ran");
}

/// Tries a TCP connection to `to` from inside `w`'s network namespace.
async fn attempt(node: &Node, w: &str, to: &str) {
    let net = node.netns.join(w).join("net");
    let (host, port) = to.split_once(':').unwrap_or_default();
    let script = format!(
        "import socket\ntry: socket.create_connection(('{host}',{port}),2)\nexcept Exception: pass"
    );
    let run = move || {
        Command::new("nsenter")
            .arg(format!("--net={}", net.display()))
            .args(["python3", "-c", &script])
            .output()
            .map(drop)
    };
    tokio::task::spawn_blocking(run).await.unwrap().unwrap();
}

/// What the rules of `chain` in the host's table commented `comment` have counted, in packets.
fn counted(chain: &str, comment: &str) -> u64 {
    let out = Command::new("nft").args(["-j", "list", "table", "inet", "korpis"]).output().unwrap();
    let listing: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let items = listing["nftables"].as_array().cloned().unwrap_or_default();
    items
        .iter()
        .filter(|i| i.pointer("/rule/chain").and_then(|c| c.as_str()) == Some(chain))
        .filter(|i| i.pointer("/rule/comment").and_then(|c| c.as_str()) == Some(comment))
        .flat_map(|i| {
            i.pointer("/rule/expr").and_then(|e| e.as_array()).cloned().unwrap_or_default()
        })
        .filter_map(|e| e.pointer("/counter/packets").and_then(serde_json::Value::as_u64))
        .sum()
}

#[tokio::test]
async fn a_web_application_reaches_its_database_at_the_address_it_was_given() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "deps-1").await;
    let agent = node.agent().await;
    let repo = push_server().await;
    let db = cp.declare(&project, "db", &repo).await;
    node.guard(&db);
    let dependency = pb::Dependency {
        name: "db".into(),
        target: Some(Target::Workload(db.clone())),
        required: true,
        ..Default::default()
    };
    let body = pb::IntentBody { dependencies: vec![dependency], ..Default::default() };
    let web = cp.declare_body(&project, "web", &repo, body).await;
    node.guard(&web);
    let other = cp.declare(&project, "other", &repo).await;
    node.guard(&other);

    let pid = running(&web).await;
    running(&other).await;
    let db_ip = address_of(&node, &db).expect("the database's address");
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
    let environ = String::from_utf8_lossy(&environ).replace('\0', "\n");
    assert!(environ.contains(&format!("KORPIS_DEP_DB_HOST={db_ip}\n")), "{environ}");
    assert!(environ.contains("KORPIS_DEP_DB_PORT=8080\n"), "{environ}");
    let file =
        std::fs::read_to_string(format!("/proc/{pid}/root/run/korpis/deps/db.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&file).unwrap();
    assert_eq!(doc["addresses"][0]["host"], db_ip.as_str());

    // Korpis's own verdict on each attempt, by its rules' counters: the connection then crosses
    // the host's forward hook, where a firewall of the operator's may still drop it (§4.1 of
    // docs/07-networking.md), as this test machine's does.
    let at = format!("{db_ip}:8080");
    let own = format!("out_{}", korpis_helper::network::host_link(&web));
    let before = counted(&own, "dependency");
    attempt(&node, &web, &at).await;
    assert!(counted(&own, "dependency") > before, "the dependent's way to its database is open");
    let before = counted("workload_forward", "other workloads");
    attempt(&node, &other, &at).await;
    assert!(counted("workload_forward", "other workloads") > before, "another workload's is not");
    for w in [&web, &other, &db] {
        cp.retire(w).await;
    }
    agent.stop().await;
}
