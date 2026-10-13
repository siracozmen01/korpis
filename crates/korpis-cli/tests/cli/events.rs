// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis event` end to end (§5 of docs/15-observability.md): subscribe, follow what comes as
//! it comes, acknowledge it, and resume after it.

use crate::support::{ControlPlane, registry};
use std::io::{BufRead as _, BufReader};
use std::process::{Command, Stdio};

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Starts `korpis event follow --ack`, and returns it with its first line.
async fn follow(cp: &ControlPlane, subscription: &str) -> (std::process::Child, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_korpis"))
        .args(["event", "follow", subscription, "--ack", "--plain-http", &registry()])
        .env("KORPIS_SERVER", &cp.url)
        .env("KORPIS_CA", cp.data.tls().join("ca.pem"))
        .env("KORPIS_TOKEN", &cp.secret)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let reader = tokio::task::spawn_blocking(move || {
        BufReader::new(stdout).lines().next().map(Result::unwrap).unwrap_or_default()
    });
    cp.project("followed").await;
    let line = tokio::time::timeout(std::time::Duration::from_secs(60), reader).await;
    (child, line.expect("a line within a minute").unwrap())
}

#[tokio::test]
async fn a_subscription_is_followed_acknowledged_and_resumed_after() {
    let cp = ControlPlane::start().await;
    let s = cp.secret.clone();
    let made = ok(&cp.korpis(&s, &["event", "subscribe", "--kind", "project."]).await);
    let id = made.split_whitespace().find(|w| w.starts_with("sub_")).unwrap().to_owned();
    assert!(ok(&cp.korpis(&s, &["event", "list"]).await).contains(&id));

    let (mut child, line) = follow(&cp, &id).await;
    assert!(line.contains("project.create") && line.starts_with("evt_"), "{line}");
    let evt = line.split_whitespace().next().unwrap().to_owned();
    let mut listed = String::new();
    for _ in 0..50 {
        listed = ok(&cp.korpis(&s, &["event", "list"]).await);
        if listed.contains(&evt) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(listed.contains(&evt), "--ack recorded what was printed: {listed}");

    assert!(ok(&cp.korpis(&s, &["event", "delete", &id]).await).contains("deleted"));
}
