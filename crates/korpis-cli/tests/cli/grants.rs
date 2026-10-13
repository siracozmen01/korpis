// SPDX-License-Identifier: AGPL-3.0-or-later

//! The CI case of §7 of docs/13-surface-cli.md, end to end: a grant issued to a new token deploys
//! one project by name, its label shows wherever it is listed, and revoking it is one row.

use crate::support::{ControlPlane, push_recipe};
use serde_json::Value;

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn a_pipeline_token_deploys_one_project_until_it_is_revoked() {
    let cp = ControlPlane::start().await;
    cp.project("games").await;
    let other = format!(r#"{{"organization": "{}", "name": "other"}}"#, cp.root_org);
    ok(&cp.korpis(&cp.secret, &["call", "ProjectService/CreateProject", "-d", &other]).await);
    let repo = push_recipe().await;
    let s = cp.secret.clone();
    let actions = "workload.read,workload.create,workload.write,project.read";
    let label = "github actions · games";
    let issue = [
        "--json",
        "grant",
        "issue",
        "--actions",
        actions,
        "--scope",
        "project:games",
        "--expires",
        "90d",
        "--label",
        label,
    ];
    let issued: Value = serde_json::from_str(&ok(&cp.korpis(&s, &issue).await)).unwrap();
    let token = issued["token"].as_str().unwrap().to_owned();
    let grant = issued["grant"]["id"].as_str().unwrap().to_owned();
    assert!(token.starts_with("korpis_"), "{issued}");
    assert_eq!(issued["grant"]["label"], label);

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sleeper.yaml");
    let text = format!(
        "workload: sleeper\nproject: games\nrecipe: {repo}:1.0.0\nconfig:\n  greeting: hi\n"
    );
    std::fs::write(&file, text).unwrap();
    ok(&cp.korpis(&token, &["apply", "-f", file.to_str().unwrap()]).await);
    assert!(ok(&cp.korpis(&token, &["get", "workloads", "games"]).await).contains("sleeper"));
    let elsewhere = cp.korpis(&token, &["get", "workloads", "other"]).await;
    assert_eq!(elsewhere.status.code(), Some(1), "another project is not even a name to it");

    let listed = ok(&cp.korpis(&s, &["grant", "list", "--scope", "project:games"]).await);
    assert!(listed.contains(label) && listed.contains(&grant), "{listed}");
    let effects = ok(&cp.korpis(&s, &["get", "effects", "--workload", "games/sleeper"]).await);
    assert!(effects.contains(&grant), "every effect it authorized names it: {effects}");

    assert!(ok(&cp.korpis(&s, &["grant", "revoke", &grant]).await).contains("revoked"));
    let after = cp.korpis(&token, &["get", "workloads", "games"]).await;
    assert!(!after.status.success(), "revoked at the next request");
}
