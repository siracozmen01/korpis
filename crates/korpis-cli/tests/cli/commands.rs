// SPDX-License-Identifier: AGPL-3.0-or-later

//! The imperative commands and the listings (§2 of docs/13-surface-cli.md): names resolve
//! through what the caller can read, each command is the API's call, and `--json` prints the
//! API's own objects.

use crate::support::{ControlPlane, push_recipe};
use serde_json::Value;

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json(out: &std::process::Output) -> Value {
    serde_json::from_str(&ok(out)).unwrap()
}

/// A plan for `sleeper` saying `greeting`, proposed and waiting: its identifier.
async fn proposed(cp: &ControlPlane, repo: &str, dir: &std::path::Path, greeting: &str) -> String {
    let path = dir.join("sleeper.yaml");
    let text = format!(
        "workload: sleeper\nproject: games\nrecipe: {repo}:1.0.0\nconfig:\n  greeting: {greeting}\n"
    );
    std::fs::write(&path, text).unwrap();
    let planned =
        json(&cp.korpis(&cp.secret, &["--json", "plan", "-f", path.to_str().unwrap()]).await);
    planned[0]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn the_commands_are_the_api_by_name() {
    let cp = ControlPlane::start().await;
    cp.project("games").await;
    let repo = push_recipe().await;
    let dir = tempfile::tempdir().unwrap();
    let s = cp.secret.clone();

    let orgs = ok(&cp.korpis(&s, &["get", "organizations"]).await);
    assert!(orgs.starts_with("ID") && orgs.contains("acme"), "{orgs}");
    assert!(ok(&cp.korpis(&s, &["get", "projects"]).await).contains("games"));

    let first = proposed(&cp, &repo, dir.path(), "hello").await;
    let approved = json(&cp.korpis(&s, &["--json", "plan", "approve", &first]).await);
    assert_eq!(approved["status"], "PLAN_STATUS_APPROVED", "{approved}");
    let second = proposed(&cp, &repo, dir.path(), "hi").await;
    let rejected =
        json(&cp.korpis(&s, &["--json", "plan", "reject", &second, "--reason", "no"]).await);
    assert_eq!(rejected["status"], "PLAN_STATUS_REJECTED", "{rejected}");

    let listed = ok(&cp.korpis(&s, &["get", "workloads", "games"]).await);
    assert!(listed.contains("sleeper"), "{listed}");
    let plans = json(&cp.korpis(&s, &["--json", "get", "plans", "games/sleeper"]).await);
    assert_eq!(plans.as_array().unwrap().len(), 2, "{plans}");
    let status = ok(&cp.korpis(&s, &["status", "sleeper"]).await);
    assert!(status.starts_with("sleeper (wl_"), "{status}");

    let stopped = json(&cp.korpis(&s, &["--json", "workload", "stop", "sleeper"]).await);
    assert_eq!(stopped["power"]["desired"], "POWER_STATE_STOPPED", "{stopped}");
    let restarted = ok(&cp.korpis(&s, &["workload", "restart", "games/sleeper"]).await);
    assert!(restarted.contains("cycle 1"), "{restarted}");

    let quiet = ["workload", "classify", "sleeper", "stop=routine", "--reason", "drained first"];
    let classified = ok(&cp.korpis(&s, &quiet).await);
    assert!(classified.contains("stop") && classified.contains("routine"), "{classified}");
    let bad =
        cp.korpis(&s, &["workload", "classify", "sleeper", "stop=quiet", "--reason", "x"]).await;
    assert_eq!(bad.status.code(), Some(1), "a class is disruptive or routine");

    let effects = ok(&cp.korpis(&s, &["get", "effects", "--workload", "sleeper"]).await);
    assert!(effects.lines().count() > 2 && effects.contains("grt_"), "named grants: {effects}");

    let token = cp
        .korpis(&s, &["node", "token", "create", "n1", "--label", "region=eu", "--expires", "30m"])
        .await;
    let token = ok(&token);
    assert!(token.contains("korpis-agent join --server https://localhost:"), "{token}");
    assert!(token.contains("--ca-fingerprint sha256:"), "{token}");
    let ghost = cp.korpis(&s, &["node", "cordon", "ghost"]).await;
    assert_eq!(ghost.status.code(), Some(1), "a name nobody can read is a local error");

    let retired = json(&cp.korpis(&s, &["--json", "workload", "delete", "sleeper"]).await);
    assert!(retired["id"].as_str().unwrap().starts_with("pln_"), "{retired}");
}
