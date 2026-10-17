// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis workload recipe`: the recipe a workload runs, its fields, and its words in the
//! language the caller asks for or its environment names (§5.3 of docs/09-recipes.md).

use crate::support::{ControlPlane, registry};

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A thin recipe with a labelled field and a Turkish catalogue; the repository.
async fn push_translated() -> String {
    let repo = format!("{}/test/cli-{}", registry(), uuid::Uuid::now_v7().simple());
    let document = r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {name: sleeper, version: "1.0.0", description: "It sleeps, and it''s quiet", license: MIT}
runtime:
  targets:
    - {tier: container, arch: [amd64, arm64], image: "docker.io/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"}
  entry: {command: [/bin/sh, -c, "exec sleep 3600"]}
preset:
  console: none
config:
  fields:
    - {key: greeting, type: string, default: hello, label: Greeting, deliver: env, deliver_as: GREETING}
"#;
    let catalogue = korpis_recipe::artifact::File {
        path: "i18n/tr.json".to_owned(),
        media_type: "application/json".to_owned(),
        data: br#"{"config.fields.greeting.label": "Selam"}"#.to_vec(),
    };
    let artifact = korpis_recipe::artifact::Artifact {
        document: document.as_bytes().to_vec(),
        files: vec![catalogue],
    };
    korpis_recipe::registry::Registry::new(vec![registry()])
        .unwrap()
        .push(&repo, "1.0.0", &artifact)
        .await
        .unwrap();
    repo
}

#[tokio::test]
async fn a_workload_s_recipe_is_shown_in_the_language_asked_for() {
    let cp = ControlPlane::start().await;
    cp.project("games").await;
    let repo = push_translated().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sleeper.yaml");
    std::fs::write(&file, format!("workload: sleeper\nproject: games\nrecipe: {repo}:1.0.0\n"))
        .unwrap();
    let s = cp.secret.clone();
    ok(&cp.korpis(&s, &["apply", "-f", file.to_str().unwrap()]).await);

    let tr = ok(&cp.korpis(&s, &["workload", "recipe", "games/sleeper", "--locale", "tr"]).await);
    assert!(tr.contains("in tr") && tr.contains("Selam") && tr.contains("greeting"), "{tr}");
    let en = ok(&cp.korpis(&s, &["workload", "recipe", "games/sleeper", "--locale", "ja"]).await);
    assert!(en.contains("in en") && en.contains("Greeting"), "{en}");
    assert!(en.contains("It sleeps, and it's quiet"), "its quoting is read: {en}");
}
