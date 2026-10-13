// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis extension` end to end (§4 of docs/16-extensions.md): the first step shows what an
//! extension needs and the command that approves exactly that digest; the second installs it;
//! uninstalling revokes what it was granted and proposes its workload's retirement.

use crate::support::{ControlPlane, registry};

const DNS: &str = include_str!("../../../korpis-recipe/tests/recipe/dns.yaml");

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

async fn push_dns() -> String {
    let repo = format!("{}/test/cli-dns-{}", registry(), uuid::Uuid::now_v7().simple());
    let artifact =
        korpis_recipe::artifact::Artifact { document: DNS.as_bytes().to_vec(), files: Vec::new() };
    korpis_recipe::registry::Registry::new(vec![registry()])
        .push(&repo, "1.0.0", &artifact)
        .await
        .unwrap();
    repo
}

#[tokio::test]
async fn an_extension_is_reviewed_then_approved_by_digest_then_uninstalled() {
    let cp = ControlPlane::start().await;
    cp.project("infra").await;
    let repo = push_dns().await;
    let s = cp.secret.clone();
    let first =
        ["extension", "install", &repo, "--version", "1.0.0", "--workload-project", "infra"];
    let review = ok(&cp.korpis(&s, &first).await);
    assert!(review.contains("endpoint.read over organization"), "{review}");
    assert!(review.contains("reaches: api.cloudflare.com:443"), "{review}");
    let approve = review.lines().find(|l| l.contains("--approve")).unwrap().trim().to_owned();
    assert!(approve.contains("--version sha256:"), "the approval names the digest read: {approve}");

    let mut by_tag = first.to_vec();
    by_tag.push("--approve");
    let refused = cp.korpis(&s, &by_tag).await;
    assert!(!refused.status.success(), "a tag may move between reading and approving");

    let args: Vec<&str> = approve.split_whitespace().skip(1).collect();
    let installed = ok(&cp.korpis(&s, &args).await);
    let id = installed.split_whitespace().find(|w| w.starts_with("ext_")).unwrap().to_owned();
    assert!(ok(&cp.korpis(&s, &["extension", "list"]).await).contains("cloudflare-dns"));
    let shown = ok(&cp.korpis(&s, &["extension", "show", &id]).await);
    assert_eq!(shown.lines().last().unwrap().matches("grt_").count(), 2, "{shown}");

    let gone = ok(&cp.korpis(&s, &["extension", "uninstall", &id]).await);
    assert!(gone.contains("retirement waits for approval"), "{gone}");
    assert!(!ok(&cp.korpis(&s, &["extension", "list"]).await).contains("cloudflare-dns"));
}
