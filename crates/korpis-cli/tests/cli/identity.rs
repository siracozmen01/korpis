// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis identity` and `korpis organization ceiling` through the binary (§5 and §5.1 of
//! docs/08-identity.md): a set is bound at an installed identity provider and shown with its
//! interval and grace together, edited against its version, asked about now, and retired; and
//! the delegation ceiling is read, raised at the root, and removed.

use crate::support::{ControlPlane, registry};

const DNS: &str = include_str!("../../../korpis-recipe/tests/recipe/dns.yaml");

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The DNS extension's document, serving `identity` as `corp` instead.
async fn push_identity() -> String {
    let document = DNS
        .replace("{type: dns, name: cloudflare}", "{type: identity, name: corp}")
        .replace("provider.dns, ", "provider.identity, ");
    let repo = format!("{}/test/cli-idp-{}", registry(), uuid::Uuid::now_v7().simple());
    let artifact =
        korpis_recipe::artifact::Artifact { document: document.into_bytes(), files: Vec::new() };
    korpis_recipe::registry::Registry::new(vec![registry()])
        .unwrap()
        .push(&repo, "1.0.0", &artifact)
        .await
        .unwrap();
    repo
}

#[tokio::test]
async fn a_set_is_bound_shown_edited_asked_about_and_retired() {
    let cp = ControlPlane::start().await;
    cp.project("infra").await;
    let s = cp.secret.clone();
    let repo = push_identity().await;
    let first =
        ["extension", "install", &repo, "--version", "1.0.0", "--workload-project", "infra"];
    let review = ok(&cp.korpis(&s, &first).await);
    let approve = review.lines().find(|l| l.contains("--approve")).unwrap().trim().to_owned();
    let args: Vec<&str> = approve.split_whitespace().skip(1).collect();
    ok(&cp.korpis(&s, &args).await);

    let bind = [
        "identity",
        "bind",
        "set:groups/admins",
        "--provider",
        "corp",
        "--interval",
        "30m",
        "--grace",
        "10m",
    ];
    let made = ok(&cp.korpis(&s, &bind).await);
    assert!(made.contains("set:groups/admins") && made.contains("unverified"), "{made}");
    assert!(made.contains("asked every 30m, with 10m of grace"), "{made}");
    assert!(made.contains("at most 40m past its last confirmation"), "{made}");
    let id = made.split_whitespace().find(|w| w.starts_with("idb_")).unwrap().to_owned();
    let listed = ok(&cp.korpis(&s, &["identity", "list"]).await);
    assert!(listed.lines().any(|l| l.contains(&id) && l.contains("corp")), "{listed}");

    let edited = ok(&cp.korpis(&s, &["identity", "edit", &id, "--interval", "1h"]).await);
    assert!(
        edited.contains("asked every 1h, with 10m of grace") && edited.contains("version 2"),
        "{edited}"
    );
    let asked = ok(&cp.korpis(&s, &["identity", "verify", &id]).await);
    assert!(asked.contains("the provider did not answer"), "nothing serves it yet: {asked}");
    let retired = ok(&cp.korpis(&s, &["identity", "retire", &id]).await);
    assert!(retired.contains("retired"), "{retired}");
}

#[tokio::test]
async fn the_delegation_ceiling_is_read_raised_at_the_root_and_removed() {
    let cp = ControlPlane::start().await;
    let s = cp.secret.clone();
    let read = ok(&cp.korpis(&s, &["organization", "ceiling"]).await);
    assert!(read.contains("at most 7d") && read.contains("its own: none"), "{read}");
    let raised = ok(&cp.korpis(&s, &["organization", "ceiling", "--set", "30d"]).await);
    assert!(raised.contains("at most 30d") && raised.contains("its own: 30d"), "{raised}");
    let past = cp.korpis(&s, &["organization", "ceiling", "--set", "91d"]).await;
    assert!(!past.status.success(), "ninety days at most");
    let removed = ok(&cp.korpis(&s, &["organization", "ceiling", "--remove"]).await);
    assert!(removed.contains("at most 7d"), "{removed}");
}
