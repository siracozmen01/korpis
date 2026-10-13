// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declared files (§3 and §4 of docs/13-surface-cli.md): a typo is a local error with its line;
//! `plan` answers with a real plan that waits; `apply --plan` applies exactly it; and a file
//! changed after its plan was read is a conflict, not a silent apply of something unread.

use crate::support::{ControlPlane, push_recipe};
use serde_json::Value;
use std::path::Path;

fn write(dir: &Path, repo: &str, greeting: &str) -> String {
    let path = dir.join("sleeper.yaml");
    let text = format!(
        "workload: sleeper\nproject: games\nrecipe: {repo}:1.0.0\nconfig:\n  greeting: {greeting}\n"
    );
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

fn plans(out: &std::process::Output) -> Vec<Value> {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice::<Vec<Value>>(&out.stdout).unwrap()
}

#[tokio::test]
async fn a_plan_is_real_and_apply_applies_exactly_it() {
    let cp = ControlPlane::start().await;
    cp.project("games").await;
    let repo = push_recipe().await;
    let dir = tempfile::tempdir().unwrap();
    let s = cp.secret.clone();

    let typo = dir.path().join("typo.yaml");
    let text =
        format!("workload: t\nproject: games\nrecipe: {repo}:1.0.0\nconfig:\n  greting: hi\n");
    std::fs::write(&typo, text).unwrap();
    let out = cp.korpis(&s, &["plan", "-f", typo.to_str().unwrap()]).await;
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{said}");
    assert!(said.contains("typo.yaml:5: config.greting"), "the file and the line: {said}");
    std::fs::remove_file(&typo).unwrap();

    let file = write(dir.path(), &repo, "hello");
    let human = cp.korpis(&s, &["plan", "-f", &file]).await;
    assert!(String::from_utf8_lossy(&human.stdout).contains("apply with:"));
    // Planning again supersedes the plan before it: the newest is the one that may apply.
    let planned = plans(&cp.korpis(&s, &["--json", "plan", "-f", &file]).await);
    assert_eq!(planned[0]["status"], "PLAN_STATUS_PROPOSED", "persisted and waiting");
    let id = planned[0]["id"].as_str().unwrap().to_owned();

    let applied = plans(&cp.korpis(&s, &["--json", "apply", "-f", &file, "--plan", &id]).await);
    assert_eq!(applied[0]["id"], id.as_str(), "that plan, and no other");
    assert_eq!(applied[0]["status"], "PLAN_STATUS_APPROVED");

    let next =
        plans(&cp.korpis(&s, &["--json", "plan", "-f", &write(dir.path(), &repo, "hi")]).await);
    let next = next[0]["id"].as_str().unwrap().to_owned();
    let moved = write(dir.path(), &repo, "bonjour");
    let conflict = cp.korpis(&s, &["apply", "-f", &moved, "--plan", &next]).await;
    assert_eq!(conflict.status.code(), Some(14), "{}", String::from_utf8_lossy(&conflict.stderr));
}
