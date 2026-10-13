// SPDX-License-Identifier: AGPL-3.0-or-later

//! Completion resolves against what the caller can read (§7 of docs/13-surface-cli.md): the
//! shell asks the binary, the binary lists through the API with the token in the environment, and
//! a token that cannot read a project completes none of its names.

use crate::support::{ControlPlane, push_recipe};
use std::process::Command;

/// What fish would offer for the last of `words`, as `value\thelp` lines.
async fn offered(cp: &ControlPlane, token: &str, words: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_korpis"));
    cmd.env("KORPIS_COMPLETE", "fish")
        .env("KORPIS_SERVER", &cp.url)
        .env("KORPIS_CA", cp.data.tls().join("ca.pem"))
        .env("KORPIS_TOKEN", token)
        .arg("--")
        .arg("korpis")
        .args(words);
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap()).await.unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn completion_offers_only_what_the_token_can_read() {
    let cp = ControlPlane::start().await;
    cp.project("games").await;
    let repo = push_recipe().await;
    let s = cp.secret.clone();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sleeper.yaml");
    let text = format!("workload: sleeper\nproject: games\nrecipe: {repo}:1.0.0\n");
    std::fs::write(&file, text).unwrap();
    let applied = cp.korpis(&s, &["apply", "-f", file.to_str().unwrap()]).await;
    assert!(applied.status.success(), "{}", String::from_utf8_lossy(&applied.stderr));

    let workloads = offered(&cp, &s, &["status", "sl"]).await;
    assert!(workloads.starts_with("sleeper\twl_"), "{workloads}");
    let scopes = offered(&cp, &s, &["grant", "issue", "--scope", "pro"]).await;
    assert_eq!(scopes.trim(), "project:", "{scopes}");
    let projects = offered(&cp, &s, &["grant", "issue", "--scope", "project:g"]).await;
    assert!(projects.starts_with("project:games\tprj_"), "{projects}");

    let nothing = offered(&cp, "korpis_not-a-token", &["status", "sl"]).await;
    assert_eq!(nothing, "", "an unknown token completes nothing, and says nothing");

    let script = std::process::Command::new(env!("CARGO_BIN_EXE_korpis"))
        .args(["completion", "bash"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&script.stdout).contains("KORPIS_COMPLETE"));
}
