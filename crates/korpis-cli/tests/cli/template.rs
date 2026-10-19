// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grant templates through the binary (§4 of docs/08-identity.md): the host defines a reseller's
//! Moderator from a file, finds it offered there by name, issues from it at a project, edits it
//! from what `template show --json` printed, finds the grant it issued behind the new version,
//! and retires it, after which it is offered nowhere and is still the reseller's to read.

use crate::support::ControlPlane;
use serde_json::{Value, json};

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn a_moderator_is_defined_issued_edited_found_behind_and_retired() {
    let cp = ControlPlane::start().await;
    let host = cp.secret.clone();
    ok(&cp.korpis(&host, &["organization", "create", "reseller"]).await);
    ok(&cp.korpis(&host, &["project", "create", "survival", "--organization", "reseller"]).await);
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, v: &Value| {
        let path = dir.path().join(name);
        std::fs::write(&path, v.to_string()).unwrap();
        path.display().to_string()
    };
    let moderator = json!({
        "name": "Moderator",
        "actions": ["workload.restart", "workload.console.read"],
        "scopePattern": { "kind": "OBJECT_KIND_PROJECT" },
        "delegation": { "allowed": true, "maxDepth": 1, "actions": ["workload.restart"] },
        "conditions": { "lifetime": "86400s" },
    });
    let file = write("moderator.json", &moderator);
    let made = ok(&cp.korpis(&host, &["template", "create", "reseller", "-f", &file]).await);
    assert!(made.contains("Moderator (tpl_") && made.contains("expires 1d after issue"), "{made}");
    let listed = ok(&cp.korpis(&host, &["template", "list", "reseller"]).await);
    assert!(listed.lines().any(|l| l.contains("Moderator") && l.contains("live")), "{listed}");

    // Issued by name, where it is offered: a grant with the template's actions, to a new token.
    let issued = ok(&cp
        .korpis(
            &host,
            &["grant", "issue", "--template", "Moderator", "--scope", "project:survival"],
        )
        .await);
    assert!(issued.contains("workload.console.read,workload.restart"), "{issued}");
    assert!(issued.contains("token, shown once"), "{issued}");

    // Edited from what `show --json` printed: the file says the version it was read at.
    let shown = ok(&cp
        .korpis(&host, &["--json", "template", "show", "Moderator", "--org", "reseller"])
        .await);
    let mut edited: Value = serde_json::from_str(&shown).unwrap();
    edited["actions"].as_array_mut().unwrap().push(json!("project.read"));
    let file = write("edited.json", &edited);
    let edit = ["template", "edit", "Moderator", "--org", "reseller", "-f", &file];
    assert!(ok(&cp.korpis(&host, &edit).await).contains("version 2"));
    let stale = cp.korpis(&host, &edit).await;
    assert_eq!(stale.status.code(), Some(14), "CONFLICT: edited from version 1 again");

    let holders =
        ok(&cp.korpis(&host, &["template", "holders", "Moderator", "--org", "reseller"]).await);
    assert!(holders.contains("1 (behind)"), "the grant from version 1 is behind: {holders}");

    ok(&cp.korpis(&host, &["template", "retire", "Moderator", "--org", "reseller"]).await);
    let offered = ok(&cp.korpis(&host, &["template", "list", "reseller"]).await);
    assert!(!offered.contains("Moderator"), "offered nowhere: {offered}");
    let owned = ok(&cp.korpis(&host, &["template", "list", "reseller", "--owned"]).await);
    assert!(owned.lines().any(|l| l.contains("Moderator") && l.contains("retired")), "{owned}");
}
