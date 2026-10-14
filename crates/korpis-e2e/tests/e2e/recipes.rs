// SPDX-License-Identifier: AGPL-3.0-or-later

//! The recipes the node tests run: a thin one that records its starts in its volume, and an
//! interactive one that answers its console.

use korpis_recipe::artifact::Artifact;
use korpis_recipe::registry::Registry;

/// The image the thin recipe runs, by digest.
pub const BUSYBOX: &str = "docker.io/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e";
/// The thin recipe's version.
pub const VERSION: &str = "1.0.0";

/// A thin recipe (§2 of docs/09-recipes.md): an image by digest, an entry that records each
/// start in its volume, a configured greeting it writes there and prints to its console, and
/// what it finds of its identity socket (§9.5 of docs/08-identity.md).
fn sleeper() -> String {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: sleeper, version: "{VERSION}", description: sleeps, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{BUSYBOX}"}}
  entry:
    command:
      - /bin/sh
      - -c
      - >-
        echo $GREETING > /data/greeting; echo said $GREETING;
        {{ test -S /run/korpis/identity.sock && echo socket;
        touch /run/korpis/x 2>/dev/null || echo read-only; }} > /data/identity;
        date +%s%N >> /data/starts; exec sleep 3600
preset:
  lifecycle: persistent
  console: logs
  volumes:
    - {{name: data, path: /data, size_default: 64Mi}}
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu:    {{min: 100m, recommended: 250m}}
config:
  fields:
    - {{key: greeting, type: string, default: hello, deliver: env, deliver_as: GREETING}}
"#
    )
}

/// A recipe with an interactive console that answers every line typed into it, as a game
/// server's console does (§6 of docs/14-streams.md).
fn answerer() -> String {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: answerer, version: "{VERSION}", description: answers, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{BUSYBOX}"}}
  entry:
    command: [/bin/sh, -c, "echo ready; while read line; do echo got $line; done"]
preset:
  lifecycle: persistent
  console: interactive
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu:    {{min: 100m, recommended: 250m}}
"#
    )
}

/// Pushes `document` to a repository no other test uses, named for `name`; the repository.
async fn push(name: &str, document: String) -> String {
    let registry =
        std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned());
    let repo = format!("{registry}/test/{name}-{}", uuid::Uuid::now_v7().simple());
    let artifact = Artifact { document: document.into_bytes(), files: Vec::new() };
    Registry::new(vec![registry]).push(&repo, VERSION, &artifact).await.unwrap();
    repo
}

/// Pushes the thin recipe to a repository no other test uses; returns the repository.
pub async fn push_sleeper() -> String {
    push("sleeper", sleeper()).await
}

/// Pushes the interactive recipe to a repository no other test uses; returns the repository.
pub async fn push_answerer() -> String {
    push("answerer", answerer()).await
}
