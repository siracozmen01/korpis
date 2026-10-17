// SPDX-License-Identifier: AGPL-3.0-or-later

//! The recipes the node tests run: a thin one that records its starts in its volume, and an
//! interactive one that answers its console.

use korpis_recipe::artifact::{Artifact, File};
use korpis_recipe::registry::Registry;

/// The test registry: the development one unless `KORPIS_TEST_REGISTRY` names another.
pub fn test_registry() -> String {
    std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned())
}

/// busybox, by digest, as `scripts/dev-registry.sh` mirrors it into the test registry: the tests
/// pull nothing from a public registry, whose limits on anonymous pulls make them fail at random.
pub fn busybox() -> String {
    format!(
        "{}/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e",
        test_registry()
    )
}
/// The thin recipe's version.
pub const VERSION: &str = "1.0.0";

/// A thin recipe (§2 of docs/09-recipes.md): an image by digest, an entry that records each
/// start in its volume, a configured greeting it writes there and prints to its console, and
/// what it finds of its identity socket (§9.5 of docs/08-identity.md).
fn sleeper() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: sleeper, version: "{VERSION}", description: sleeps, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
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
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: answerer, version: "{VERSION}", description: answers, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
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

/// An extension providing a backup target named `s3` (§3 of docs/16-extensions.md) whose
/// container only sleeps: the test serves the provider itself, with the credential the node hands
/// the extension's workload through its identity socket, as the container would.
fn target() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Extension
metadata: {{name: s3-target, version: "{VERSION}", description: backups, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
  entry:
    command: [/bin/sh, -c, "exec sleep 3600"]
preset:
  lifecycle: persistent
  console: none
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu: {{min: 50m, recommended: 100m}}
extension:
  api_version: korpis.extension.v1
  requires: [provider.serve, provider.backup_target]
  provides:
    - {{type: backup_target, name: s3}}
"#
    )
}

/// A recipe running the test web program in `image`; its endpoints are the intent's to declare.
fn web(image: &str) -> String {
    let entry = crate::image::TEST_WEB.entry();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: web, version: "{VERSION}", description: serves, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{image}"}}
  entry:
    command: [{entry}]
preset:
  lifecycle: persistent
  console: logs
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu:    {{min: 50m, recommended: 100m}}
"#
    )
}

/// Pushes the web recipe over the test web program's image to a repository no other test uses;
/// the repository.
pub async fn push_web() -> String {
    let image = crate::image::push_image(&test_registry(), crate::image::TEST_WEB).await;
    push("web", web(&image)).await
}

/// Pushes the backup target extension to a repository no other test uses; the repository.
pub async fn push_target() -> String {
    push("s3-target", target()).await
}

/// Pushes `document` to a repository no other test uses, named for `name`; the repository.
pub async fn push(name: &str, document: String) -> String {
    push_with(name, document, Vec::new()).await
}

/// Pushes `document` with `files` to a repository no other test uses, named for `name`; the
/// repository.
pub async fn push_with(name: &str, document: String, files: Vec<File>) -> String {
    let registry = test_registry();
    let repo = format!("{registry}/test/{name}-{}", uuid::Uuid::now_v7().simple());
    let artifact = Artifact { document: document.into_bytes(), files };
    Registry::new(vec![registry]).unwrap().push(&repo, VERSION, &artifact).await.unwrap();
    repo
}

/// Pushes the thin recipe to a repository no other test uses; returns the repository.
pub async fn push_sleeper() -> String {
    push("sleeper", sleeper()).await
}

/// A server: busybox's httpd answering `served` on 8080, which an endpoint declares and the
/// node exposes on a port of its own, and on 8081, which nothing declares.
fn server() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: server, version: "{VERSION}", description: serves, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
  entry:
    command:
      - /bin/sh
      - -c
      - >-
        mkdir -p /tmp/www && echo served > /tmp/www/index.html &&
        httpd -p 8081 -h /tmp/www && exec httpd -f -p 8080 -h /tmp/www
preset:
  lifecycle: persistent
  console: logs
  endpoints:
    - {{name: http, protocol: tcp, port: 8080, exposure_default: node_port}}
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu:    {{min: 50m, recommended: 100m}}
"#
    )
}

/// Pushes the server recipe to a repository no other test uses; returns the repository.
pub async fn push_server() -> String {
    push("server", server()).await
}

/// Pushes the interactive recipe to a repository no other test uses; returns the repository.
pub async fn push_answerer() -> String {
    push("answerer", answerer()).await
}
