// SPDX-License-Identifier: AGPL-3.0-or-later

//! Trace 10 of docs/23-walkthroughs.md through the CLI (§5 and §6 of docs/13-surface-cli.md): a
//! set is applied, a person changes a member at 03:00, the 09:00 apply fails whole naming the
//! change, `--accept-drift` applies the rest, and a member removed from the files is proposed for
//! deletion by `plan` and deleted only by the `apply --plan` that names it.

use crate::support::{ControlPlane, push_recipe};
use std::path::Path;

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn file(dir: &Path, name: &str, repo: &str, extra: &str) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{name}.yaml"));
    let text = format!("workload: {name}\nproject: games\nrecipe: {repo}:1.0.0\n{extra}");
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

#[tokio::test]
async fn the_fix_at_three_and_the_pipeline_at_nine() {
    let cp = ControlPlane::start().await;
    cp.project("games").await;
    let repo = push_recipe().await;
    let s = cp.secret.clone();
    let root = tempfile::tempdir().unwrap();
    let servers = root.path().join("servers");
    let a = file(&servers, "alpha", &repo, "");
    file(&servers, "beta", &repo, "");
    let dir = servers.display().to_string();
    ok(&cp.korpis(&s, &["apply", "-f", &dir, "--set", "servers"]).await);
    assert!(ok(&cp.korpis(&s, &["set", "list", "games"]).await).contains("advisory"));

    let fix =
        file(&root.path().join("fix"), "beta", &repo, "resources:\n  memory:\n    limit: 256Mi\n");
    ok(&cp.korpis(&s, &["apply", "-f", &fix]).await);
    let status = ok(&cp.korpis(&s, &["status", "games/beta"]).await);
    assert!(status.contains("drift:") && status.contains("effect eff_"), "{status}");
    assert!(
        status.contains("memory limit (bytes): the set's files say 0, now 268435456"),
        "{status}"
    );

    let nine = cp.korpis(&s, &["apply", "-f", &dir, "--set", "servers"]).await;
    let said = String::from_utf8_lossy(&nine.stderr);
    assert_eq!(nine.status.code(), Some(14), "{said}");
    assert!(said.contains("beta") && said.contains("nothing was applied"), "{said}");
    let accepted =
        cp.korpis(&s, &["apply", "-f", &dir, "--set", "servers", "--accept-drift"]).await;
    assert!(ok(&accepted).contains("left as it is: beta"));

    std::fs::remove_file(&a).unwrap();
    let flags = ["--set", "servers", "--reconcile"];
    let planned = ok(&cp.korpis(&s, &[&["plan", "-f", &dir][..], &flags].concat()).await);
    let line = planned.lines().find(|l| l.starts_with("apply with:")).unwrap();
    assert!(line.contains("--set servers --reconcile --plan"), "{planned}");
    let still = ok(&cp.korpis(&s, &["get", "workloads", "games"]).await);
    assert!(still.contains("alpha"), "plan proposes a deletion, never performs one");
    let args: Vec<&str> =
        line.trim_start_matches("apply with:").split_whitespace().skip(1).collect();
    ok(&cp.korpis(&s, &args).await);
    let gone = cp.korpis(&s, &["status", "games/beta"]).await;
    assert!(!ok(&gone).contains("drift:"), "reconciled");
    let effects = ok(&cp.korpis(&s, &["get", "effects", "--workload", "games/beta"]).await);
    assert!(effects.contains("set.drift_discarded"), "{effects}");

    let volumes =
        cp.korpis(&s, &["apply", "-f", &dir, "--set", "servers", "--delete-volumes", "data"]).await;
    assert_eq!(volumes.status.code(), Some(1), "volume deletion is not Phase 0's");
    ok(&cp.korpis(&s, &["set", "mode", "games/servers", "strict"]).await);
    assert!(ok(&cp.korpis(&s, &["set", "list", "games"]).await).contains("strict"));
}
